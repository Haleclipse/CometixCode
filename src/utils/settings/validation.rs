//! Settings validation against the carrier SettingsSchema.
//!
//! Maps to: CC `utils/settings/validation.ts`.
//!
//! | validation.ts                     | Rust                                  |
//! |-----------------------------------|---------------------------------------|
//! | `formatZodError()`                | `format_zod_error()`                  |
//! | `validateSettingsFileContent()`   | `validate_settings_file_content()`    |
//! | `filterInvalidPermissionRules()`  | `filter_invalid_permission_rules()`   |
//! | `ValidationError` type            | `ValidationError` struct              |
//! | `SettingsWithErrors` type         | `SettingsWithErrors` struct           |
//!
//! `validate_settings` is the load-time aggregation the settings loader
//! (`settings.ts#parseSettingsFile` territory) drives: raw-JSON permission
//! rule filtering first, then a `SettingsSchema` safe_parse whose failures
//! flow through `format_zod_error`. Schema failure returns no settings,
//! preserving the nullable source-file result from settings.ts:201-231.

use super::permission_validation::validate_permission_rule;
use super::schema_output::generate_settings_json_schema;
use super::types::{SettingsJson, settings_schema};
use super::validation_tips::{TipContext, get_validation_tip};
use crate::utils::string_utils::plural;
use crate::utils::zod::{self, Issue, IssueCode, PathSegment};

/// Maps to: CC `ValidationError`.
#[derive(Debug, Clone)]
pub struct ValidationError {
    pub file: Option<String>,
    /// Field path in dot notation (e.g., "permissions.defaultMode").
    pub path: String,
    pub message: String,
    pub expected: Option<String>,
    pub invalid_value: Option<String>,
    /// Maps to: CC `utils/settings/validation.ts` `ValidationError.docLink`.
    pub doc_link: Option<String>,
    pub suggestion: Option<String>,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(ref file) = self.file {
            write!(f, "[{}] ", file)?;
        }
        write!(f, "{}: {}", self.path, self.message)?;
        if let Some(ref suggestion) = self.suggestion {
            write!(f, " ({})", suggestion)?;
        }
        Ok(())
    }
}

/// `Default` exists purely so `Arc<SettingsWithErrors>` can be a typed
/// component prop (iocraft `Props` requires `Default`); it is the empty
/// in-memory shape, never a substitute for `getSettingsWithAllErrors()`.
#[derive(Clone, Default)]
pub struct SettingsWithErrors {
    pub settings: SettingsJson,
    pub errors: Vec<ValidationError>,
    /// Source-specific policy settings, when available. This mirrors official
    /// `getSettingsForSource('policySettings')` for hook/status-line gating
    /// without changing the merged settings shape.
    pub policy_settings: Option<SettingsJson>,
}

// ════════════════════════════════════════════════════════════
// formatZodError
// ════════════════════════════════════════════════════════════

/// Maps to: CC `extractReceivedFromMessage(msg)` — `/received (\w+)/`.
fn extract_received_from_message(msg: &str) -> Option<&str> {
    let rest = &msg[msg.find("received ")? + "received ".len()..];
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    (end > 0).then(|| &rest[..end])
}

/// A numeric bound as CC's `String(minimum)` writes it.
fn bound_string(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

/// Maps to: CC `formatZodError(error, filePath)` — one `ValidationError` per
/// issue, with the special-cased copy for invalid_value / invalid_type /
/// unrecognized_keys / too_small and the contextual tip lookup.
pub fn format_zod_error(error: &zod::ZodError, file_path: &str) -> Vec<ValidationError> {
    error
        .issues
        .iter()
        .map(|issue| format_zod_issue(issue, file_path))
        .collect()
}

fn format_zod_issue(issue: &Issue, file_path: &str) -> ValidationError {
    let path = issue
        .path
        .iter()
        .map(|seg| match seg {
            PathSegment::Key(k) => k.clone(),
            PathSegment::Index(i) => i.to_string(),
        })
        .collect::<Vec<_>>()
        .join(".");
    let mut message = issue.message.clone();
    let mut expected: Option<String> = None;

    let mut enum_values: Option<Vec<String>> = None;
    let mut expected_value: Option<String> = None;
    let mut received_value: Option<String> = None;
    let mut invalid_value: Option<String> = None;

    match issue.code {
        IssueCode::InvalidValue => {
            let values: Vec<String> = issue
                .values
                .iter()
                .flatten()
                .map(|v| match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect();
            expected_value = Some(values.join(" | "));
            enum_values = Some(values);
        }
        IssueCode::InvalidType => {
            expected_value = issue.expected.map(str::to_string);
            let received = extract_received_from_message(&issue.message).map(str::to_string);
            received_value = received.clone();
            invalid_value = received;
        }
        IssueCode::TooSmall => {
            expected_value = issue.minimum.map(bound_string);
        }
        IssueCode::Custom => {
            if let Some(params) = &issue.params {
                let received = params.get("received").map(|v| match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                });
                received_value = received.clone();
                invalid_value = received;
            }
        }
        _ => {}
    }

    let tip = get_validation_tip(&TipContext {
        path: &path,
        code: issue.code.as_str(),
        expected: expected_value.as_deref(),
        received: received_value.as_deref(),
        enum_values: enum_values.as_deref(),
    });

    match issue.code {
        IssueCode::InvalidValue => {
            let quoted = enum_values
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|v| format!("\"{v}\""))
                .collect::<Vec<_>>()
                .join(", ");
            message = format!("Invalid value. Expected one of: {quoted}");
            expected = Some(quoted);
        }
        IssueCode::InvalidType => {
            let received_type = received_value.as_deref().unwrap_or("unknown");
            if issue.expected == Some("object") && received_type == "null" && path.is_empty() {
                message = "Invalid or malformed JSON".to_string();
            } else {
                message = format!(
                    "Expected {}, but received {received_type}",
                    issue.expected.unwrap_or("unknown")
                );
            }
        }
        IssueCode::UnrecognizedKeys => {
            let keys = issue.keys.join(", ");
            message = format!(
                "Unrecognized {}: {keys}",
                plural(issue.keys.len(), "field", None)
            );
        }
        IssueCode::TooSmall => {
            if let Some(minimum) = issue.minimum {
                message = format!(
                    "Number must be greater than or equal to {}",
                    bound_string(minimum)
                );
                expected = Some(bound_string(minimum));
            }
        }
        _ => {}
    }

    let tip = tip.unwrap_or_default();
    ValidationError {
        file: Some(file_path.to_string()),
        path,
        message,
        expected,
        invalid_value,
        doc_link: tip.doc_link,
        suggestion: tip.suggestion,
    }
}

// ════════════════════════════════════════════════════════════
// validateSettingsFileContent
// ════════════════════════════════════════════════════════════

/// Maps to: CC `validateSettingsFileContent`'s return shape.
pub enum SettingsFileValidation {
    Valid,
    Invalid { error: String, full_schema: String },
}

/// Maps to: CC `validateSettingsFileContent(content)` — used during file
/// edits to ensure the resulting file is valid. Validates against
/// `SettingsSchema().strict()`: the passthrough root becomes strict so typos
/// in top-level keys are caught here even though normal loading admits them.
pub fn validate_settings_file_content(content: &str) -> SettingsFileValidation {
    let json_data: serde_json::Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(parse_error) => {
            // The parser's own message stands in for the JS parser's
            // (deliberate copy deviation: the text comes from the parser).
            return SettingsFileValidation::Invalid {
                error: format!("Invalid JSON: {parse_error}"),
                full_schema: generate_settings_json_schema(),
            };
        }
    };

    match zod::safe_parse(&settings_schema().clone().strict(), &json_data) {
        Ok(_) => SettingsFileValidation::Valid,
        Err(zod_error) => {
            let errors = format_zod_error(&zod_error, "settings");
            let error_message = format!(
                "Settings validation failed:\n{}",
                errors
                    .iter()
                    .map(|err| format!("- {}: {}", err.path, err.message))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            SettingsFileValidation::Invalid {
                error: error_message,
                full_schema: generate_settings_json_schema(),
            }
        }
    }
}

// ════════════════════════════════════════════════════════════
// filterInvalidPermissionRules
// ════════════════════════════════════════════════════════════

/// Maps to: CC `filterInvalidPermissionRules(data, filePath)` — filters
/// invalid permission rules from raw parsed JSON BEFORE schema validation, so
/// one bad rule cannot poison the entire settings file. Mutates `data` and
/// returns a warning per removed rule. (Empty arrays stay in place — CC keeps
/// the filtered array whatever its length.)
pub fn filter_invalid_permission_rules(
    data: &mut serde_json::Value,
    file_path: Option<&str>,
) -> Vec<ValidationError> {
    let Some(perms) = data
        .get_mut("permissions")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Vec::new();
    };

    let mut warnings = Vec::new();
    for key in ["allow", "deny", "ask"] {
        let Some(rules) = perms.get_mut(key).and_then(serde_json::Value::as_array_mut) else {
            continue;
        };
        rules.retain(|rule| {
            let Some(rule_str) = rule.as_str() else {
                warnings.push(ValidationError {
                    file: file_path.map(str::to_string),
                    path: format!("permissions.{key}"),
                    message: format!("Non-string value in {key} array was removed"),
                    expected: None,
                    invalid_value: Some(rule.to_string()),
                    doc_link: None,
                    suggestion: None,
                });
                return false;
            };
            let result = validate_permission_rule(rule_str);
            if !result.valid {
                let mut message = format!("Invalid permission rule \"{rule_str}\" was skipped");
                if let Some(error) = &result.error {
                    message.push_str(&format!(": {error}"));
                }
                if let Some(suggestion) = &result.suggestion {
                    message.push_str(&format!(". {suggestion}"));
                }
                warnings.push(ValidationError {
                    file: file_path.map(str::to_string),
                    path: format!("permissions.{key}"),
                    message,
                    expected: None,
                    invalid_value: Some(rule_str.to_string()),
                    doc_link: None,
                    suggestion: None,
                });
                return false;
            }
            true
        });
    }
    warnings
}

// ════════════════════════════════════════════════════════════
// Load-time aggregation
// ════════════════════════════════════════════════════════════

/// Maps to: CC settings.ts:201-231 `parseSettingsFileUncached` validation arm.
/// The optional settings value belongs to a single source file; the merged
/// `SettingsWithErrors` above remains non-null. `Err` covers JSON/projection
/// errors, while schema failures retain their formatted validation errors.
pub fn validate_settings(
    content: &str,
    file_path: Option<&str>,
) -> Result<(Option<SettingsJson>, Vec<ValidationError>), String> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut raw: serde_json::Value = serde_json::from_str(content).map_err(|e| {
        format!(
            "JSON parse error: {}",
            e.to_string().lines().next().unwrap_or("unknown error")
        )
    })?;

    // CC's load order: filter bad permission rules on the RAW data first.
    let mut errors = filter_invalid_permission_rules(&mut raw, file_path);

    match zod::safe_parse(settings_schema(), &raw) {
        Ok(data) => {
            // The serde struct is the post-parse projection: it deserializes
            // from safe_parse's DATA (coerce/catch/strip applied), not the
            // original input.
            let settings: SettingsJson = serde_json::from_value(data)
                .map_err(|e| format!("settings projection failed: {e}"))?;
            Ok((Some(settings), errors))
        }
        Err(zod_error) => {
            errors.extend(format_zod_error(
                &zod_error,
                file_path.unwrap_or("settings"),
            ));
            Ok((None, errors))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn settings_validation_matches_official_nullable_source_and_warning_order() {
        let (settings, errors) = validate_settings(
            r#"{"language":42,"permissions":{"allow":["Read","Bash()"]}}"#,
            Some("settings.json"),
        )
        .unwrap();
        assert!(settings.is_none());
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].path, "permissions.allow");
        assert_eq!(errors[1].path, "language");
        let (settings, errors) = validate_settings(
            r#"{"language":"zh","permissions":{"allow":["Read","Bash()"]}}"#,
            Some("settings.json"),
        )
        .unwrap();
        assert_eq!(
            settings.unwrap().permissions.unwrap().allow,
            Some(vec!["Read".into()])
        );
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn format_zod_error_rewrites_copy_like_official() {
        // invalid_value → "Invalid value. Expected one of: ..." with the
        // defaultMode tip attached.
        let error = zod::safe_parse(
            settings_schema(),
            &json!({"permissions": {"defaultMode": "yolo"}}),
        )
        .expect_err("bad mode fails");
        let formatted = format_zod_error(&error, "settings");
        let mode_error = &formatted[0];
        assert_eq!(mode_error.path, "permissions.defaultMode");
        assert!(
            mode_error
                .message
                .starts_with("Invalid value. Expected one of: \"acceptEdits\"")
        );
        assert_eq!(
            mode_error.suggestion.as_deref(),
            Some(
                "Valid modes: \"acceptEdits\" (ask before file changes), \"plan\" (analysis only), \"bypassPermissions\" (auto-accept all), or \"default\" (standard behavior)"
            )
        );
        assert_eq!(
            mode_error.doc_link.as_deref(),
            Some("https://code.claude.com/docs/en/iam#permission-modes")
        );

        // invalid_type → "Expected X, but received Y".
        let error = zod::safe_parse(settings_schema(), &json!({"language": 5}))
            .expect_err("number language fails");
        let formatted = format_zod_error(&error, "settings");
        assert_eq!(formatted[0].message, "Expected string, but received number");
        assert_eq!(formatted[0].invalid_value.as_deref(), Some("number"));

        // too_small → the numeric floor copy plus the cleanupPeriodDays tip.
        let error = zod::safe_parse(settings_schema(), &json!({"cleanupPeriodDays": -1}))
            .expect_err("negative days fail");
        let formatted = format_zod_error(&error, "settings");
        assert_eq!(
            formatted[0].message,
            "Number must be greater than or equal to 0"
        );
        assert!(
            formatted[0]
                .suggestion
                .as_deref()
                .is_some_and(|s| s.starts_with("Must be 0 or greater. Set a positive number"))
        );
    }

    #[test]
    fn validate_settings_file_content_is_strict_like_official() {
        // Valid content passes.
        assert!(matches!(
            validate_settings_file_content(r#"{"language": "zh"}"#),
            SettingsFileValidation::Valid
        ));
        // The edit-time validator applies .strict(): unknown top-level keys
        // fail here even though normal loading passes them through.
        let SettingsFileValidation::Invalid { error, full_schema } =
            validate_settings_file_content(r#"{"modle": "opus"}"#)
        else {
            panic!("typo key should fail strict validation");
        };
        assert!(error.starts_with("Settings validation failed:\n- "));
        assert!(error.contains("Unrecognized field: modle"));
        assert!(full_schema.contains("\"$schema\""));
        // Broken JSON reports the parse failure.
        assert!(matches!(
            validate_settings_file_content("{"),
            SettingsFileValidation::Invalid { error, .. } if error.starts_with("Invalid JSON: ")
        ));
    }

    #[test]
    fn filter_invalid_permission_rules_skips_with_official_copy() {
        let mut data = json!({
            "permissions": {
                "allow": ["Bash(ls)", "Bash()", 42],
                "deny": ["Read(src/**)"],
            },
        });
        let warnings = filter_invalid_permission_rules(&mut data, Some("settings"));
        assert_eq!(warnings.len(), 2);
        assert_eq!(
            warnings[0].message,
            "Invalid permission rule \"Bash()\" was skipped: Empty parentheses. Either specify a pattern or use just \"Bash\" without parentheses"
        );
        assert_eq!(
            warnings[1].message,
            "Non-string value in allow array was removed"
        );
        // The bad entries are gone, the good ones stay, arrays stay in place.
        assert_eq!(data["permissions"]["allow"], json!(["Bash(ls)"]));
        assert_eq!(data["permissions"]["deny"], json!(["Read(src/**)"]));

        // A filtered file then parses cleanly end-to-end.
        let (settings, errors) = validate_settings(
            r#"{"permissions": {"allow": ["Bash(ls)", "Bash()"]}}"#,
            Some("settings"),
        )
        .expect("parses");
        assert_eq!(errors.len(), 1);
        assert_eq!(
            settings.unwrap().permissions.as_ref().unwrap().allow,
            Some(vec!["Bash(ls)".to_string()])
        );
    }
}
