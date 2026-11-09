//! Maps to: CC `keybindings/validate.ts`.

use super::parser::{chord_to_string, parse_chord};
use super::types::{KeybindingBlock, KeybindingWarningSeverity, ParsedBinding};
use regex::Regex;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

/// Maps to: CC `KeybindingWarningType`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum KeybindingWarningType {
    #[default]
    ParseError,
    Duplicate,
    Reserved,
    InvalidContext,
    InvalidAction,
}

impl KeybindingWarningType {
    fn from_source_name(value: &str) -> Self {
        match value {
            "duplicate" => Self::Duplicate,
            "reserved" => Self::Reserved,
            "invalid_context" => Self::InvalidContext,
            "invalid_action" => Self::InvalidAction,
            _ => Self::ParseError,
        }
    }
}

/// Maps to: CC `KeybindingWarning`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeybindingWarning {
    pub warning_type: KeybindingWarningType,
    pub severity: KeybindingWarningSeverity,
    pub message: String,
    pub key: Option<String>,
    pub context: Option<String>,
    pub action: Option<String>,
    pub suggestion: Option<String>,
}

impl KeybindingWarning {
    pub fn error(message: impl Into<String>, suggestion: Option<impl Into<String>>) -> Self {
        Self {
            severity: KeybindingWarningSeverity::Error,
            message: message.into(),
            suggestion: suggestion.map(Into::into),
            ..Self::default()
        }
    }

    pub fn warning(message: impl Into<String>, suggestion: Option<impl Into<String>>) -> Self {
        Self {
            severity: KeybindingWarningSeverity::Warning,
            message: message.into(),
            suggestion: suggestion.map(Into::into),
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug)]
struct Warning {
    kind: &'static str,
    key: Option<String>,
    context: Option<String>,
    item: KeybindingWarning,
}

fn warning(
    kind: &'static str,
    severity: KeybindingWarningSeverity,
    message: impl Into<String>,
    key: Option<&str>,
    context: Option<&str>,
    suggestion: Option<String>,
) -> Warning {
    Warning {
        kind,
        key: key.map(str::to_string),
        context: context.map(str::to_string),
        item: KeybindingWarning {
            warning_type: KeybindingWarningType::from_source_name(kind),
            severity,
            message: message.into(),
            key: key.map(str::to_string),
            context: context.map(str::to_string),
            action: None,
            suggestion,
        },
    }
}

fn warning_with_action(
    kind: &'static str,
    severity: KeybindingWarningSeverity,
    message: impl Into<String>,
    key: Option<&str>,
    context: Option<&str>,
    action: Option<&str>,
    suggestion: Option<String>,
) -> Warning {
    let mut warning = warning(kind, severity, message, key, context, suggestion);
    warning.item.action = action.map(str::to_string);
    warning
}

fn normalize_key(key: &str) -> String {
    super::reserved_shortcuts::normalize_key_for_comparison(key)
}

fn validate_keystroke(key: &str, context: Option<&str>) -> Option<Warning> {
    if key.split('+').any(|part| part.trim().is_empty()) {
        return Some(warning(
            "parse_error",
            KeybindingWarningSeverity::Error,
            format!("Empty key part in \"{key}\""),
            Some(key),
            context,
            Some("Remove extra \"+\" characters".to_string()),
        ));
    }
    let chord = parse_chord(key);
    if chord.is_empty()
        || chord.iter().all(|stroke| {
            // CC intentionally does not count `super` here: a modifier-only
            // `cmd`/`super` token still has no key and is invalid.
            stroke.key.is_empty() && !stroke.ctrl && !stroke.alt && !stroke.shift && !stroke.meta
        })
    {
        return Some(warning(
            "parse_error",
            KeybindingWarningSeverity::Error,
            format!("Could not parse keystroke \"{key}\""),
            Some(key),
            context,
            None,
        ));
    }
    None
}

fn check_duplicate_keys_in_json_internal(content: &str) -> Vec<Warning> {
    static BLOCK_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?s)"bindings"\s*:\s*\{([^{}]*(?:\{[^{}]*\}[^{}]*)*)\}"#)
            .expect("bindings block regex")
    });
    static CONTEXT_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#""context"\s*:\s*"([^"]+)"[^{]*$"#).expect("context regex"));
    static KEY_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#""([^"]+)"\s*:"#).expect("binding key regex"));
    let mut warnings = Vec::new();
    for captures in BLOCK_RE.captures_iter(content) {
        let block_start = captures.get(0).map_or(0, |capture| capture.start());
        let context = CONTEXT_RE
            .captures(&content[..block_start])
            .and_then(|captures| captures.get(1))
            .map(|capture| capture.as_str())
            .unwrap_or("unknown");
        let body = captures
            .get(1)
            .map(|capture| capture.as_str())
            .unwrap_or("");
        let mut counts = HashMap::<&str, usize>::new();
        for captures in KEY_RE.captures_iter(body) {
            let Some(key) = captures.get(1).map(|capture| capture.as_str()) else {
                continue;
            };
            let count = counts.entry(key).or_default();
            *count += 1;
            if *count == 2 {
                warnings.push(warning(
                    "duplicate",
                    KeybindingWarningSeverity::Warning,
                    format!("Duplicate key \"{key}\" in {context} bindings"),
                    Some(key),
                    Some(context),
                    Some("This key appears multiple times in the same context. JSON uses the last value, earlier values are ignored.".to_string()),
                ));
            }
        }
    }
    warnings
}

/// Runs the same user-only structural, duplicate, command, and reserved
/// checks as CC after the object-wrapper shape has been accepted by the loader.
pub fn validate_user_bindings(content: &str, blocks: &[Value]) -> Vec<KeybindingWarning> {
    let mut warnings = check_duplicate_keys_in_json_internal(content);
    let reserved_shortcuts = super::reserved_shortcuts::get_reserved_shortcuts();
    let mut seen_by_context = HashMap::<String, HashMap<String, String>>::new();

    for (index, block) in blocks.iter().enumerate() {
        let Some(object) = block.as_object() else {
            warnings.push(warning(
                "parse_error",
                KeybindingWarningSeverity::Error,
                format!("Keybinding block {} is not an object", index + 1),
                None,
                None,
                None,
            ));
            continue;
        };
        let raw_context = object.get("context").and_then(Value::as_str);
        let valid_context = raw_context.filter(|context| super::schema::is_valid_context(context));
        if let Some(context) = raw_context {
            if valid_context.is_none() {
                warnings.push(warning(
                    "invalid_context",
                    KeybindingWarningSeverity::Error,
                    format!("Unknown context \"{context}\""),
                    None,
                    Some(context),
                    Some(format!(
                        "Valid contexts: {}",
                        super::schema::KEYBINDING_CONTEXTS.join(", ")
                    )),
                ));
            }
        } else {
            warnings.push(warning(
                "parse_error",
                KeybindingWarningSeverity::Error,
                format!("Keybinding block {} missing \"context\" field", index + 1),
                None,
                None,
                None,
            ));
        }
        let Some(bindings) = object.get("bindings").and_then(Value::as_object) else {
            warnings.push(warning(
                "parse_error",
                KeybindingWarningSeverity::Error,
                format!("Keybinding block {} missing \"bindings\" field", index + 1),
                None,
                valid_context,
                None,
            ));
            continue;
        };
        let context_for_duplicates = raw_context.unwrap_or_default().to_string();
        let context_map = seen_by_context
            .entry(context_for_duplicates.clone())
            .or_default();

        for (key, raw_action) in bindings {
            if let Some(key_warning) = validate_keystroke(key, valid_context) {
                warnings.push(key_warning);
            }
            let action = match raw_action {
                Value::String(action) => Some(action.as_str()),
                Value::Null => None,
                _ => {
                    warnings.push(warning(
                        "invalid_action",
                        KeybindingWarningSeverity::Error,
                        format!("Invalid action for \"{key}\": must be a string or null"),
                        Some(key),
                        valid_context,
                        None,
                    ));
                    continue;
                }
            };

            if let Some(action) = action {
                if action.starts_with("command:") {
                    if !super::schema::is_command_action(action) {
                        warnings.push(warning_with_action(
                            "invalid_action",
                            KeybindingWarningSeverity::Warning,
                            format!("Invalid command binding \"{action}\" for \"{key}\": command name may only contain alphanumeric characters, colons, hyphens, and underscores"),
                            Some(key),
                            valid_context,
                            Some(action),
                            None,
                        ));
                    }
                    if let Some(context) = valid_context.filter(|context| *context != "Chat") {
                        warnings.push(warning_with_action(
                            "invalid_action",
                            KeybindingWarningSeverity::Warning,
                            format!("Command binding \"{action}\" must be in \"Chat\" context, not \"{context}\""),
                            Some(key),
                            Some(context),
                            Some(action),
                            Some("Move this binding to a block with \"context\": \"Chat\"".to_string()),
                        ));
                    }
                }
            }

            let normalized = normalize_key(key);
            let action_for_duplicate = action.unwrap_or("null").to_string();
            if let Some(previous) = context_map.get(&normalized)
                && previous != &action_for_duplicate
            {
                warnings.push(warning_with_action(
                    "duplicate",
                    KeybindingWarningSeverity::Warning,
                    format!("Duplicate binding \"{key}\" in {context_for_duplicates} context"),
                    Some(key),
                    raw_context,
                    Some(action.unwrap_or("null (unbind)")),
                    Some(format!(
                        "Previously bound to \"{previous}\". Only the last binding will be used."
                    )),
                ));
            }
            context_map.insert(normalized.clone(), action_for_duplicate);

            if let Some(reserved) = reserved_shortcuts.iter().find(|reserved| {
                super::reserved_shortcuts::normalize_key_for_comparison(reserved.key) == normalized
            }) {
                let display = chord_to_string(&parse_chord(key));
                warnings.push(warning_with_action(
                    "reserved",
                    reserved.severity,
                    format!("\"{display}\" may not work: {}", reserved.reason),
                    Some(&display),
                    raw_context,
                    action,
                    None,
                ));
            }
        }
    }

    let mut seen = HashSet::new();
    warnings
        .into_iter()
        .filter(|warning| seen.insert((warning.kind, warning.key.clone(), warning.context.clone())))
        .map(|warning| warning.item)
        .collect()
}

/// Maps to: CC `checkDuplicateKeysInJson(...)`.
pub fn check_duplicate_keys_in_json(content: &str) -> Vec<KeybindingWarning> {
    check_duplicate_keys_in_json_internal(content)
        .into_iter()
        .map(|warning| warning.item)
        .collect()
}

/// Maps to: CC `validateUserConfig(...)`.
pub fn validate_user_config(user_blocks: &Value) -> Vec<KeybindingWarning> {
    let Some(blocks) = user_blocks.as_array() else {
        return vec![KeybindingWarning {
            warning_type: KeybindingWarningType::ParseError,
            severity: KeybindingWarningSeverity::Error,
            message: "keybindings.json must contain an array".to_string(),
            suggestion: Some("Wrap your bindings in [ ]".to_string()),
            ..KeybindingWarning::default()
        }];
    };
    validate_user_bindings("", blocks)
        .into_iter()
        .filter(|warning| {
            !matches!(
                warning.warning_type,
                KeybindingWarningType::Duplicate | KeybindingWarningType::Reserved
            )
        })
        .collect()
}

/// Maps to: CC `checkDuplicates(...)`.
pub fn check_duplicates(user_blocks: &[KeybindingBlock]) -> Vec<KeybindingWarning> {
    let mut seen_by_context = HashMap::<String, HashMap<String, String>>::new();
    let mut warnings = Vec::new();
    for block in user_blocks {
        let context = block.context.as_official_str().to_string();
        let context_map = seen_by_context.entry(context.clone()).or_default();
        for (key, action) in &block.bindings {
            let normalized = normalize_key(key);
            let current_action = action.as_deref().unwrap_or("null");
            if let Some(existing) = context_map.get(&normalized)
                && !existing.is_empty()
                && existing != current_action
            {
                warnings.push(KeybindingWarning {
                    warning_type: KeybindingWarningType::Duplicate,
                    severity: KeybindingWarningSeverity::Warning,
                    message: format!("Duplicate binding \"{key}\" in {context} context"),
                    key: Some(key.clone()),
                    context: Some(context.clone()),
                    action: Some(
                        action
                            .clone()
                            .unwrap_or_else(|| "null (unbind)".to_string()),
                    ),
                    suggestion: Some(format!(
                        "Previously bound to \"{existing}\". Only the last binding will be used."
                    )),
                });
            }
            context_map.insert(normalized, current_action.to_string());
        }
    }
    warnings
}

/// Maps to: CC `checkReservedShortcuts(...)`.
pub fn check_reserved_shortcuts(bindings: &[ParsedBinding]) -> Vec<KeybindingWarning> {
    let reserved = super::reserved_shortcuts::get_reserved_shortcuts();
    let mut warnings = Vec::new();
    for binding in bindings {
        let key_display = chord_to_string(&binding.chord);
        let normalized = normalize_key(&key_display);
        let Some(shortcut) = reserved.iter().find(|shortcut| {
            super::reserved_shortcuts::normalize_key_for_comparison(shortcut.key) == normalized
        }) else {
            continue;
        };
        warnings.push(KeybindingWarning {
            warning_type: KeybindingWarningType::Reserved,
            severity: shortcut.severity,
            message: format!("\"{key_display}\" may not work: {}", shortcut.reason),
            key: Some(key_display),
            context: Some(binding.context.as_official_str().to_string()),
            action: binding.action.clone(),
            suggestion: None,
        });
    }
    warnings
}

/// Maps to: CC `validateBindings(...)`. The parsed merged snapshot is retained
/// in the signature for source parity; validation intentionally targets only
/// user blocks, as CC does.
pub fn validate_bindings(
    user_blocks: &Value,
    _parsed_bindings: &[ParsedBinding],
) -> Vec<KeybindingWarning> {
    let Some(blocks) = user_blocks.as_array() else {
        return validate_user_config(user_blocks);
    };
    validate_user_bindings("", blocks)
}

/// Maps to: CC `formatWarning(...)`.
pub fn format_warning(warning: &KeybindingWarning) -> String {
    let icon = if warning.severity == KeybindingWarningSeverity::Error {
        '✗'
    } else {
        '⚠'
    };
    let severity = if warning.severity == KeybindingWarningSeverity::Error {
        "error"
    } else {
        "warning"
    };
    let mut message = format!("{icon} Keybinding {severity}: {}", warning.message);
    if let Some(suggestion) = &warning.suggestion {
        message.push_str("\n  ");
        message.push_str(suggestion);
    }
    message
}

/// Maps to: CC `formatWarnings(...)`.
pub fn format_warnings(warnings: &[KeybindingWarning]) -> String {
    if warnings.is_empty() {
        return String::new();
    }
    let errors = warnings
        .iter()
        .filter(|warning| warning.severity == KeybindingWarningSeverity::Error)
        .collect::<Vec<_>>();
    let warning_items = warnings
        .iter()
        .filter(|warning| warning.severity == KeybindingWarningSeverity::Warning)
        .collect::<Vec<_>>();
    let plural = |count: usize, noun: &str| {
        if count == 1 {
            noun.to_string()
        } else {
            format!("{noun}s")
        }
    };
    let mut sections = Vec::new();
    if !errors.is_empty() {
        let mut lines = vec![format!(
            "Found {} keybinding {}:",
            errors.len(),
            plural(errors.len(), "error")
        )];
        lines.extend(errors.into_iter().map(format_warning));
        sections.push(lines.join("\n"));
    }
    if !warning_items.is_empty() {
        let mut lines = vec![format!(
            "Found {} keybinding {}:",
            warning_items.len(),
            plural(warning_items.len(), "warning")
        )];
        lines.extend(warning_items.into_iter().map(format_warning));
        sections.push(lines.join("\n"));
    }
    sections.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_matches_reserved_command_context_and_duplicate_copy() {
        let content = r#"{
          "bindings": [
            {"context":"Global","bindings":{"ctrl+c":"app:redraw","x":"command:bad name","y":"command:help"}},
            {"context":"Global","bindings":{"x":"app:redraw"}}
          ]
        }"#;
        let value: Value = serde_json::from_str(content).unwrap();
        let blocks = value["bindings"].as_array().unwrap();
        let warnings = validate_user_bindings(content, blocks);
        assert!(warnings.iter().any(|warning| {
            warning
                .message
                .contains("\"ctrl+c\" may not work: Cannot be rebound")
        }));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.message.contains("must be in \"Chat\" context"))
        );
        assert!(warnings.iter().any(|warning| {
            warning
                .message
                .contains("Invalid command binding \"command:bad name\"")
        }));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.message == "Duplicate binding \"x\" in Global context")
        );
    }

    #[test]
    fn public_validation_and_format_boundaries_match_official_copy() {
        let blocks = serde_json::json!([{
            "context": "Global",
            "bindings": {"ctrl+c": "app:redraw"}
        }]);
        let warnings = validate_bindings(&blocks, &[]);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].warning_type, KeybindingWarningType::Reserved);
        assert_eq!(
            format_warning(&warnings[0]),
            "✗ Keybinding error: \"ctrl+c\" may not work: Cannot be rebound - used for interrupt/exit (hardcoded)"
        );
        assert!(format_warnings(&warnings).starts_with("Found 1 keybinding error:\n"));

        let shape = validate_user_config(&serde_json::json!({"context": "Chat"}));
        assert_eq!(shape[0].message, "keybindings.json must contain an array");
        assert_eq!(
            shape[0].suggestion.as_deref(),
            Some("Wrap your bindings in [ ]")
        );
        let malformed = validate_user_config(&serde_json::json!([
            "not-an-object",
            {"context": "Chat"},
            {"context": "Chat", "bindings": {"cmd": "chat:submit"}}
        ]));
        assert!(
            malformed
                .iter()
                .any(|warning| warning.message == "Keybinding block 1 is not an object")
        );
        assert!(
            malformed
                .iter()
                .any(|warning| warning.message == "Keybinding block 2 missing \"bindings\" field")
        );
        assert!(
            malformed
                .iter()
                .any(|warning| warning.message == "Could not parse keystroke \"cmd\"")
        );
    }

    #[test]
    fn check_duplicates_preserves_current_action_metadata() {
        let blocks = vec![KeybindingBlock {
            context: crate::keybindings::types::ContextName::Chat,
            bindings: vec![
                ("ctrl+x".to_string(), Some("chat:stash".to_string())),
                ("control+x".to_string(), Some("chat:undo".to_string())),
            ],
            include_in_template: true,
        }];
        let warnings = check_duplicates(&blocks);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].action.as_deref(), Some("chat:undo"));
        assert_eq!(warnings[0].context.as_deref(), Some("Chat"));
    }

    #[test]
    fn duplicate_json_key_warns_before_json_last_wins() {
        let content = r#"{"bindings":[{"context":"Chat","bindings":{"enter":"chat:submit","enter":"chat:cancel"}}]}"#;
        let value: Value = serde_json::from_str(content).unwrap();
        let warnings = validate_user_bindings(content, value["bindings"].as_array().unwrap());
        assert!(
            warnings
                .iter()
                .any(|warning| warning.message == "Duplicate key \"enter\" in Chat bindings")
        );
    }
}
