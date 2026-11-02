//! Maps to: CC `tools/MCPTool/UI.tsx` (complete file).
//!
//! `feature('MCP_RICH_OUTPUT')` is `true` in the shipped build
//! (`rebuild/scripts/build.ts:59`), so the rich path is unconditional here and
//! CC's `:170` / `:192` plain-`OutputLine` alternatives are dead code. Do not
//! "restore" them.

use crate::components::messages::user_tool_result_message::utils::{
    ToolRenderLine, ToolRenderOptions, ToolRenderSegment, ToolRenderTone,
};
use crate::components::shell::output_line::linkify_urls_in_text;
use crate::constants::figures::figures;
use crate::utils::hyperlink::create_hyperlink;
use crate::utils::zod::js_string;
use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;
use unicode_width::UnicodeWidthStr;

const MCP_OUTPUT_WARNING_THRESHOLD_TOKENS: usize = 10_000;
const MAX_INPUT_VALUE_CHARS: usize = 80;
const MAX_FLAT_JSON_KEYS: usize = 12;
const MAX_FLAT_JSON_CHARS: usize = 5_000;
const MAX_JSON_PARSE_CHARS: usize = 200_000;
const UNWRAP_MIN_STRING_LEN: usize = 200;

fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

/// Maps to: the property order `Object.entries` yields (ES `OrdinaryOwnPropertyKeys`)
/// at CC `UI.tsx:54` and `:279` — integer-index keys first in ascending numeric
/// order, then the remaining string keys in insertion order. `serde_json`'s
/// `preserve_order` map keeps pure insertion order, so `{"b":1,"2":2,"1":3}`
/// would otherwise render its rows in a different order than CC.
fn js_object_entries(object: &serde_json::Map<String, Value>) -> Vec<(&String, &Value)> {
    let mut indices: Vec<(u32, &String, &Value)> = Vec::new();
    let mut names: Vec<(&String, &Value)> = Vec::new();
    for (key, value) in object {
        match array_index_key(key) {
            Some(index) => indices.push((index, key, value)),
            None => names.push((key, value)),
        }
    }
    indices.sort_by_key(|(index, _, _)| *index);
    indices
        .into_iter()
        .map(|(_, key, value)| (key, value))
        .chain(names)
        .collect()
}

/// An ES "array index": a canonical numeric string below `2^32 - 1`. `"01"`,
/// `"1.5"`, `"-1"` and `"4294967295"` stay ordinary string keys.
fn array_index_key(key: &str) -> Option<u32> {
    if key.is_empty() || !key.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if key.len() > 1 && key.starts_with('0') {
        return None;
    }
    key.parse::<u32>().ok().filter(|index| *index != u32::MAX)
}

fn take_utf16(value: &str, max_units: usize) -> String {
    let mut units = 0usize;
    value
        .chars()
        .take_while(|character| {
            let width = character.len_utf16();
            if units + width > max_units {
                return false;
            }
            units += width;
            true
        })
        .collect()
}

/// Maps to: CC `tools/MCPTool/UI.tsx:47-67` `renderToolUseMessage`.
pub fn render_tool_use_message(input: &Value, verbose: bool) -> Option<String> {
    let input = input.as_object()?;
    if input.is_empty() {
        return Some(String::new());
    }
    Some(
        js_object_entries(input)
            .into_iter()
            .map(|(key, value)| {
                let mut rendered = serde_json::to_string(value).unwrap_or_else(|_| "null".into());
                if !verbose && utf16_len(&rendered) > MAX_INPUT_VALUE_CHARS {
                    rendered = format!(
                        "{}…",
                        take_utf16(&rendered, MAX_INPUT_VALUE_CHARS).trim_end()
                    );
                }
                format!("{key}: {rendered}")
            })
            .collect::<Vec<_>>()
            .join(", "),
    )
}

/// Maps to: CC `tools/MCPTool/UI.tsx:115-215` `renderToolResultMessage`.
/// L1 (`React/Ink -> iocraft`): React nodes are projected to ordered render
/// lines while preserving output variants, warning order, and compact branches.
pub fn render_tool_result_message(
    output: &Value,
    input: Option<&Value>,
    options: ToolRenderOptions,
) -> Vec<ToolRenderLine> {
    if !options.verbose
        && let Some((channel, url)) = try_slack_send_compact(output, input)
    {
        return vec![ToolRenderLine::new(
            format!(
                "Sent a message to {}",
                create_hyperlink(&url, Some(&channel))
            ),
            ToolRenderTone::Normal,
        )];
    }

    let estimated_tokens = crate::utils::mcp_validation::get_content_size_estimate(output);
    let mut lines = Vec::new();
    if estimated_tokens > MCP_OUTPUT_WARNING_THRESHOLD_TOKENS {
        lines.push(ToolRenderLine::new(
            format!(
                "{} Large MCP response (~{} tokens), this can fill up context quickly",
                figures().warning,
                crate::utils::format::format_number(estimated_tokens as u64)
            ),
            ToolRenderTone::Warning,
        ));
    }

    match output {
        Value::Array(items) => {
            for item in items {
                if item.get("type").and_then(Value::as_str) == Some("image") {
                    lines.push(ToolRenderLine::new("[Image]", ToolRenderTone::Normal));
                    continue;
                }
                // CC `:160-166`: `String(item.text)` once the `!== null` and
                // `!== undefined` guards pass, otherwise `''`.
                let text = if item.get("type").and_then(Value::as_str) == Some("text") {
                    item.get("text")
                        .filter(|text| !text.is_null())
                        .map(js_string)
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                lines.extend(mcp_text_output(&text, options));
            }
        }
        // CC `:180 else if (!mcpOutput)` is JS truthiness, not a null/empty
        // pair: `0`, `false` and `NaN` reach "(No content)" too. They sit
        // outside the declared `string | MCPToolResult` domain, but the Rust
        // carrier is `Value`, so the branch is ported as CC wrote it rather
        // than narrowed to the two members the type admits.
        value if is_js_falsy(value) => lines.push(ToolRenderLine::new(
            "(No content)",
            ToolRenderTone::Inactive,
        )),
        Value::String(content) => lines.extend(mcp_text_output(content, options)),
        other => lines.extend(mcp_text_output(&js_string(other), options)),
    }

    lines
}

/// JS truthiness for the `!mcpOutput` guard: `null`, `false`, `0`/`-0`, `NaN`
/// (not representable in `serde_json`) and `''` are falsy.
fn is_js_falsy(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(false) => true,
        Value::Bool(true) => false,
        Value::Number(number) => number.as_f64().is_none_or(|number| number == 0.0),
        Value::String(text) => text.is_empty(),
        Value::Array(_) | Value::Object(_) => false,
    }
}

/// Maps to: CC `tools/MCPTool/UI.tsx:217-256` `MCPTextOutput`.
fn mcp_text_output(content: &str, options: ToolRenderOptions) -> Vec<ToolRenderLine> {
    if let Some(unwrapped) = try_unwrap_text_payload(content) {
        let mut lines = Vec::new();
        if !unwrapped.extras.is_empty() {
            lines.push(ToolRenderLine::new(
                unwrapped
                    .extras
                    .iter()
                    .map(|(key, value)| format!("{key}: {value}"))
                    .collect::<Vec<_>>()
                    .join(" · "),
                ToolRenderTone::Inactive,
            ));
        }
        lines.extend(output_line(&unwrapped.body, options));
        return lines;
    }

    if let Some(flat) = try_flatten_json(content) {
        // CC `:241` takes the max with `stringWidth(k)` (display columns) but
        // `:247` pads with `key.padEnd(maxKeyWidth)` (UTF-16 code units). The
        // two metrics only agree for narrow-BMP keys; a CJK key over-pads by
        // its width surplus. Replicated, not corrected: this is what CC ships,
        // and MCP servers key their JSON in ASCII in practice.
        let max_key_width = flat
            .iter()
            .map(|(key, _)| UnicodeWidthStr::width(key.as_str()))
            .max()
            .unwrap_or(0);
        return flat
            .into_iter()
            .map(|(key, value)| {
                let padding = max_key_width.saturating_sub(utf16_len(&key));
                let padded_key = format!("{}{}", key, " ".repeat(padding));
                let value = linkify_urls_in_text(&value);
                ToolRenderLine::new(format!("{padded_key}: {value}"), ToolRenderTone::Normal)
                    .with_segments(vec![
                        ToolRenderSegment::new(format!("{padded_key}: ")).with_dim(true),
                        ToolRenderSegment::new(value),
                    ])
            })
            .collect();
    }

    output_line(content, options)
}

/// L1 (`React/Ink -> iocraft`) line projection of the shared
/// `components/shell/OutputLine.tsx#OutputLine` child used by MCP UI. Both MCP
/// mount points pass `linkifyUrls` (CC `:234`, `:255`), so the linkify pass is
/// unconditional here.
///
/// CC `OutputLine.tsx:74-85` order: format → linkify → truncate →
/// `stripUnderlineAnsi`, with the strip on BOTH branches (`:80`, `:82-84`).
/// It is the leak defense documented at `:98-104`, and MCP servers are exactly
/// the untrusted ANSI source it hardens against.
fn output_line(content: &str, options: ToolRenderOptions) -> Vec<ToolRenderLine> {
    let formatted = crate::components::shell::output_line::try_json_format_content_public(content);
    let formatted = linkify_urls_in_text(&formatted);
    if options.verbose {
        return vec![ToolRenderLine::new(
            crate::tools::bash_tool::ui::strip_underline_ansi(&formatted),
            ToolRenderTone::Normal,
        )];
    }

    let truncated =
        crate::tools::bash_tool::ui::render_truncated_content(&formatted, options.terminal_width);
    let mut lines = Vec::new();
    // CC's `OutputLine` always renders its `MessageResponse`, even when the
    // formatted content is empty (`:89-95`) — whitespace-only MCP content
    // still costs one blank row.
    lines.push(ToolRenderLine::new(
        crate::tools::bash_tool::ui::strip_underline_ansi(&truncated.above_the_fold),
        ToolRenderTone::Normal,
    ));
    if truncated.remaining_lines > 0 {
        lines.push(
            ToolRenderLine::new(
                format!(
                    "… +{} lines {}",
                    truncated.remaining_lines,
                    crate::components::ctrl_o_to_expand::ctrl_o_to_expand_hint()
                ),
                ToolRenderTone::Inactive,
            )
            .with_dim(true),
        );
    }
    lines
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnwrappedTextPayload {
    pub body: String,
    pub extras: Vec<(String, String)>,
}

/// Maps to: CC `tools/MCPTool/UI.tsx:262-283` `parseJsonEntries`.
fn parse_json_entries(
    content: &str,
    max_chars: usize,
    max_keys: usize,
) -> Option<Vec<(String, Value)>> {
    let trimmed = content.trim();
    if trimmed.is_empty() || utf16_len(trimmed) > max_chars || !trimmed.starts_with('{') {
        return None;
    }
    let parsed: Value = serde_json::from_str(trimmed).ok()?;
    let object = parsed.as_object()?;
    if object.is_empty() || object.len() > max_keys {
        return None;
    }
    Some(
        js_object_entries(object)
            .into_iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    )
}

/// Maps to: CC `tools/MCPTool/UI.tsx:291-315` `tryFlattenJson`.
pub fn try_flatten_json(content: &str) -> Option<Vec<(String, String)>> {
    let entries = parse_json_entries(content, MAX_FLAT_JSON_CHARS, MAX_FLAT_JSON_KEYS)?;
    let mut result = Vec::new();
    for (key, value) in entries {
        let rendered = match value {
            Value::String(text) => text,
            // CC `:301-306`: `String(value)` for null/number/boolean — the
            // number path is `Number::toString`, so `1` not serde's `1.0`.
            Value::Null | Value::Bool(_) | Value::Number(_) => js_string(&value),
            Value::Object(_) | Value::Array(_) => {
                let compact = serde_json::to_string(&value).ok()?;
                if utf16_len(&compact) > 120 {
                    return None;
                }
                compact
            }
        };
        result.push((key, rendered));
    }
    Some(result)
}

/// Maps to: CC `tools/MCPTool/UI.tsx:324-359` `tryUnwrapTextPayload`.
pub fn try_unwrap_text_payload(content: &str) -> Option<UnwrappedTextPayload> {
    let entries = parse_json_entries(content, MAX_JSON_PARSE_CHARS, 4)?;
    let mut body = None;
    let mut extras = Vec::new();

    for (key, value) in entries {
        match value {
            Value::String(text) => {
                let trimmed = text.trim_end().to_string();
                let length = utf16_len(&trimmed);
                let is_dominant =
                    length > UNWRAP_MIN_STRING_LEN || (trimmed.contains('\n') && length > 50);
                if is_dominant {
                    if body.is_some() {
                        return None;
                    }
                    body = Some(trimmed);
                    continue;
                }
                if length > 150 {
                    return None;
                }
                extras.push((key, collapse_whitespace(&trimmed)));
            }
            // CC `:348-353`: `String(value)` for null/number/boolean.
            Value::Null | Value::Bool(_) | Value::Number(_) => {
                extras.push((key, js_string(&value)))
            }
            Value::Object(_) | Value::Array(_) => return None,
        }
    }

    Some(UnwrappedTextPayload {
        body: body?,
        extras,
    })
}

fn collapse_whitespace(value: &str) -> String {
    static WHITESPACE: OnceLock<Regex> = OnceLock::new();
    WHITESPACE
        .get_or_init(|| Regex::new(r"\s+").expect("whitespace regex is valid"))
        .replace_all(value, " ")
        .to_string()
}

const SLACK_ARCHIVES_RE: &str = r"^https://[a-z0-9-]+\.slack\.com/archives/([A-Z0-9]+)/p[0-9]+$";

/// Maps to: CC `tools/MCPTool/UI.tsx:372-395` `trySlackSendCompact`.
pub fn try_slack_send_compact(output: &Value, input: Option<&Value>) -> Option<(String, String)> {
    let text = match output {
        Value::Array(blocks) => blocks
            .iter()
            .find(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .and_then(|block| block.get("text")),
        value => Some(value),
    }?;
    let text = text.as_str()?;
    if !text.contains("\"message_link\"") {
        return None;
    }

    let entries = parse_json_entries(text, 2_000, 6)?;
    let url = entries
        .iter()
        .find_map(|(key, value)| (key == "message_link").then_some(value))?
        .as_str()?;
    let regex = slack_archives_regex();
    let fallback_channel = regex.captures(url)?.get(1)?.as_str();
    let raw_label = input.and_then(|input| {
        input
            .get("channel_id")
            .filter(|value| !value.is_null())
            .or_else(|| input.get("channel").filter(|value| !value.is_null()))
    });
    let raw_label = match raw_label {
        Some(Value::String(label)) if !label.is_empty() => label.as_str(),
        Some(_) => "slack",
        None => fallback_channel,
    };
    let label = if raw_label.starts_with('#') {
        raw_label.to_string()
    } else {
        format!("#{raw_label}")
    };
    Some((label, url.to_string()))
}

fn slack_archives_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(SLACK_ARCHIVES_RE).expect("Slack URL regex is valid"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn options(verbose: bool) -> ToolRenderOptions {
        ToolRenderOptions {
            verbose,
            terminal_width: 200,
            ..ToolRenderOptions::default()
        }
    }

    #[test]
    fn mcp_result_empty_and_array_branches_match_official_ui() {
        let empty = render_tool_result_message(&Value::String(String::new()), None, options(false));
        assert_eq!(empty[0].text, "(No content)");
        assert_eq!(empty[0].tone, ToolRenderTone::Inactive);

        let array = render_tool_result_message(
            &json!([
                {"type": "text", "text": "caption"},
                {"type": "image", "source": {"type": "base64"}},
                {"type": "resource", "text": "must not leak"}
            ]),
            None,
            options(true),
        );
        assert_eq!(array[0].text, "caption");
        assert_eq!(array[1].text, "[Image]");
        assert_eq!(array[2].text, "");
        assert!(render_tool_result_message(&json!([]), None, options(false)).is_empty());
    }

    #[test]
    fn mcp_result_large_output_matches_official_compact_number() {
        let content = "x".repeat(40_004);
        let lines =
            render_tool_result_message(&Value::String(content.clone()), None, options(true));
        assert!(lines[0].text.contains("Large MCP response"));
        assert!(lines[0].text.contains("10.0k tokens"));
        assert_eq!(lines[0].tone, ToolRenderTone::Warning);
        assert_eq!(lines[1].text, content);
    }

    #[test]
    fn mcp_rich_output_flattens_small_json_like_official_ui() {
        let lines = render_tool_result_message(
            &json!(r#"{"ok":true,"url":"https://example.test","meta":{"page":1}}"#),
            None,
            options(false),
        );
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().any(|line| line.text == "ok  : true"));
        let url_line = lines
            .iter()
            .find(|line| line.text.contains("url"))
            .expect("url line");
        assert_eq!(url_line.segments[0].text, "url : ");
        assert!(url_line.segments[0].dim);
        assert!(url_line.text.contains("https://example.test"));
        assert!(lines.iter().any(|line| line.text == r#"meta: {"page":1}"#));
    }

    #[test]
    fn mcp_rich_output_unwraps_dominant_text_payload_like_official_ui() {
        let body = format!("{}\nsecond line", "a".repeat(60));
        let content = json!({"cursor": "next page", "messages": body}).to_string();
        let lines = render_tool_result_message(&Value::String(content), None, options(true));
        assert_eq!(lines[0].text, "cursor: next page");
        assert_eq!(lines[0].tone, ToolRenderTone::Inactive);
        assert_eq!(lines[1].text, format!("{}\nsecond line", "a".repeat(60)));
    }

    #[test]
    fn mcp_slack_send_compact_prefers_input_channel_like_official_ui() {
        let content = json!(
            r##"{"message_link":"https://acme.slack.com/archives/C123ABC/p1712345678901234","ok":true}"##
        );
        let input = json!({"channel": "ops"});
        let lines = render_tool_result_message(&content, Some(&input), options(false));
        assert_eq!(lines.len(), 1);
        assert!(lines[0].text.starts_with("Sent a message to "));
        assert!(
            lines[0].text.contains("#ops")
                || lines[0]
                    .text
                    .contains("https://acme.slack.com/archives/C123ABC/p1712345678901234")
        );

        let verbose_lines = render_tool_result_message(&content, Some(&input), options(true));
        assert!(
            verbose_lines
                .iter()
                .any(|line| line.text.contains("message_link"))
        );

        assert_eq!(
            try_slack_send_compact(
                &content,
                Some(&json!({"channel_id": null, "channel": "fallback"}))
            )
            .map(|(channel, _)| channel),
            Some("#fallback".to_string())
        );
        assert_eq!(
            try_slack_send_compact(
                &content,
                Some(&json!({"channel_id": 42, "channel": "ignored"}))
            )
            .map(|(channel, _)| channel),
            Some("#slack".to_string())
        );
    }

    #[test]
    fn mcp_text_block_stringifies_like_official_string_call() {
        // CC `:165` `String(item.text)` — objects are `[object Object]`,
        // arrays comma-join, and numbers go through `Number::toString`.
        let lines = render_tool_result_message(
            &json!([
                {"type": "text", "text": {"a": 1}},
                {"type": "text", "text": [1, 2]},
                {"type": "text", "text": 1.0},
                {"type": "text", "text": true},
            ]),
            None,
            options(true),
        );
        assert_eq!(lines[0].text, "[object Object]");
        assert_eq!(lines[1].text, "1,2");
        assert_eq!(lines[2].text, "1");
        assert_eq!(lines[3].text, "true");
    }

    #[test]
    fn mcp_scalar_values_stringify_like_official_string_call() {
        // CC `:306` and `:351` `String(value)` on the null/number/boolean
        // branch. `1.0` is `1` in JS, never serde_json's `1.0`.
        assert_eq!(
            try_flatten_json(r#"{"a":1.0,"b":2.5,"c":null,"d":false}"#),
            Some(vec![
                ("a".to_string(), "1".to_string()),
                ("b".to_string(), "2.5".to_string()),
                ("c".to_string(), "null".to_string()),
                ("d".to_string(), "false".to_string()),
            ])
        );

        let body = "x".repeat(220);
        let content = format!(r#"{{"count":3.0,"messages":"{body}"}}"#);
        assert_eq!(
            try_unwrap_text_payload(&content).map(|payload| payload.extras),
            Some(vec![("count".to_string(), "3".to_string())])
        );
    }

    #[test]
    fn mcp_output_strips_underline_ansi_on_both_branches() {
        // CC `OutputLine.tsx:79-85` strips on the full AND the truncated
        // branch; `:98-104` documents why. MCP servers are external, so this
        // is the leak path the defense exists for.
        let leaky =
            Value::String("\u{1b}[4munderlined\u{1b}[24m \u{1b}[31mred\u{1b}[39m".to_string());
        for verbose in [true, false] {
            let lines = render_tool_result_message(&leaky, None, options(verbose));
            assert!(
                !lines[0].text.contains("\u{1b}[4m"),
                "verbose={verbose} text={:?}",
                lines[0].text
            );
            // Colors are deliberately kept (`OutputLine.tsx:101-103`).
            assert!(
                lines[0].text.contains("\u{1b}[31m"),
                "verbose={verbose} text={:?}",
                lines[0].text
            );
            assert!(lines[0].text.contains("underlined"));
        }
    }

    #[test]
    fn mcp_flat_json_pads_keys_with_the_official_metric_mismatch() {
        // CC `:241` measures with `stringWidth` but `:247` pads with
        // `padEnd` (UTF-16 units): a CJK key gets `maxKeyWidth - key.length`
        // spaces, which over-pads by its display-width surplus. Replicated.
        let lines =
            render_tool_result_message(&json!(r#"{"日本":"a","ab":"b"}"#), None, options(false));
        assert_eq!(lines[0].text, "日本  : a");
        assert_eq!(lines[1].text, "ab  : b");
    }

    #[test]
    fn mcp_object_entries_hoist_integer_like_keys_like_js() {
        // `Object.entries({b:1,"2":2,"1":3})` is `[["1",3],["2",2],["b",1]]`.
        let lines = render_tool_result_message(
            &json!(r#"{"b":"one","2":"two","1":"three","01":"kept"}"#),
            None,
            options(false),
        );
        let texts: Vec<&str> = lines.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(texts, vec!["1 : three", "2 : two", "b : one", "01: kept"]);
    }

    #[test]
    fn mcp_falsy_output_renders_no_content_like_js_truthiness() {
        // CC `:180 !mcpOutput`.
        for falsy in [json!(""), json!(null), json!(0), json!(false)] {
            let lines = render_tool_result_message(&falsy, None, options(false));
            assert_eq!(lines.len(), 1, "value={falsy}");
            assert_eq!(lines[0].text, "(No content)", "value={falsy}");
            assert_eq!(lines[0].tone, ToolRenderTone::Inactive, "value={falsy}");
        }
        // Truthy scalars fall through to the text renderer.
        let truthy = render_tool_result_message(&json!(true), None, options(false));
        assert_eq!(truthy[0].text, "true");
    }

    #[test]
    fn mcp_whitespace_only_output_still_costs_a_row() {
        // CC's OutputLine always renders its MessageResponse (`:89-95`), even
        // when `renderTruncatedContent` trimmed the content to nothing.
        let lines = render_tool_result_message(&json!("   \n  "), None, options(false));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "");
    }

    #[test]
    fn mcp_tool_use_message_matches_official_compact_and_verbose_values() {
        let input = json!({"query": "x".repeat(100)});
        let compact = render_tool_use_message(&input, false).expect("summary");
        assert!(compact.ends_with('…'));
        assert!(utf16_len(compact.split_once(": ").unwrap().1) <= 81);
        let verbose = render_tool_use_message(&input, true).expect("summary");
        assert!(!verbose.ends_with('…'));
        assert_eq!(
            render_tool_use_message(&json!({}), false).as_deref(),
            Some("")
        );
    }
}
