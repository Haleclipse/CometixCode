//! Elicitation form validation helpers.
//! Maps to: CC `utils/mcp/elicitationValidation.ts`.
//!
//! Intentional divergence: CC `validateElicitationInputAsync(...)` can call the
//! Haiku-backed natural-language date parser from `dateTimeParser.ts`. Nyan
//! keeps this helper network/API safe for now and returns the synchronous
//! validation result when the input is not already ISO-compatible. The future
//! Skill/MCP runtime slice should wire the official date parser boundary here.

use chrono::{DateTime, NaiveDate};
use regex::Regex;
use serde_json::{Number, Value};
use std::sync::OnceLock;

/// Maps to: CC `ValidationResult` in `utils/mcp/elicitationValidation.ts`.
#[derive(Clone, Debug, PartialEq)]
pub struct ValidationResult {
    pub value: Option<Value>,
    pub is_valid: bool,
    pub error: Option<String>,
}

impl ValidationResult {
    fn valid(value: Value) -> Self {
        Self {
            value: Some(value),
            is_valid: true,
            error: None,
        }
    }

    fn invalid(error: impl Into<String>) -> Self {
        Self {
            value: None,
            is_valid: false,
            error: Some(error.into()),
        }
    }
}

fn schema_type(schema: &Value) -> Option<&str> {
    schema.get("type").and_then(Value::as_str)
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn const_title_array(value: Option<&Value>, key: &str) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get(key).and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Maps to: CC `isEnumSchema(...)`.
pub fn is_enum_schema(schema: &Value) -> bool {
    schema_type(schema) == Some("string")
        && (schema.get("enum").is_some() || schema.get("oneOf").is_some())
}

/// Maps to: CC `isMultiSelectEnumSchema(...)`.
pub fn is_multi_select_enum_schema(schema: &Value) -> bool {
    schema_type(schema) == Some("array")
        && schema.get("items").is_some_and(|items| {
            items.is_object() && (items.get("enum").is_some() || items.get("anyOf").is_some())
        })
}

/// Maps to: CC `getMultiSelectValues(...)`.
pub fn get_multi_select_values(schema: &Value) -> Vec<String> {
    let Some(items) = schema.get("items") else {
        return Vec::new();
    };
    let any_of_values = const_title_array(items.get("anyOf"), "const");
    if !any_of_values.is_empty() {
        return any_of_values;
    }
    string_array(items.get("enum"))
}

/// Maps to: CC `getMultiSelectLabels(...)`.
pub fn get_multi_select_labels(schema: &Value) -> Vec<String> {
    let Some(items) = schema.get("items") else {
        return Vec::new();
    };
    let any_of_labels = const_title_array(items.get("anyOf"), "title");
    if !any_of_labels.is_empty() {
        return any_of_labels;
    }
    string_array(items.get("enum"))
}

/// Maps to: CC `getMultiSelectLabel(...)`.
pub fn get_multi_select_label(schema: &Value, value: &str) -> String {
    let values = get_multi_select_values(schema);
    let labels = get_multi_select_labels(schema);
    values
        .iter()
        .position(|candidate| candidate == value)
        .and_then(|idx| labels.get(idx).cloned())
        .unwrap_or_else(|| value.to_string())
}

/// Maps to: CC `getEnumValues(...)`.
pub fn get_enum_values(schema: &Value) -> Vec<String> {
    let one_of_values = const_title_array(schema.get("oneOf"), "const");
    if !one_of_values.is_empty() {
        return one_of_values;
    }
    string_array(schema.get("enum"))
}

/// Maps to: CC `getEnumLabels(...)`.
pub fn get_enum_labels(schema: &Value) -> Vec<String> {
    let one_of_labels = const_title_array(schema.get("oneOf"), "title");
    if !one_of_labels.is_empty() {
        return one_of_labels;
    }
    let enum_names = string_array(schema.get("enumNames"));
    if !enum_names.is_empty() {
        return enum_names;
    }
    string_array(schema.get("enum"))
}

/// Maps to: CC `getEnumLabel(...)`.
pub fn get_enum_label(schema: &Value, value: &str) -> String {
    let values = get_enum_values(schema);
    let labels = get_enum_labels(schema);
    values
        .iter()
        .position(|candidate| candidate == value)
        .and_then(|idx| labels.get(idx).cloned())
        .unwrap_or_else(|| value.to_string())
}

fn plural(count: usize, singular: &str) -> String {
    if count == 1 {
        singular.to_string()
    } else {
        format!("{singular}s")
    }
}

fn format_num(value: f64, is_integer_schema: bool) -> String {
    if value.fract() == 0.0 && !is_integer_schema {
        format!("{value:.1}")
    } else {
        let mut text = value.to_string();
        if text.ends_with(".0") {
            text.truncate(text.len() - 2);
        }
        text
    }
}

fn number_range_message(schema: &Value, is_integer_schema: bool) -> String {
    let type_label = if is_integer_schema {
        "an integer"
    } else {
        "a number"
    };
    let minimum = schema.get("minimum").and_then(Value::as_f64);
    let maximum = schema.get("maximum").and_then(Value::as_f64);
    match (minimum, maximum) {
        (Some(min), Some(max)) => format!(
            "Must be {type_label} between {} and {}",
            format_num(min, is_integer_schema),
            format_num(max, is_integer_schema)
        ),
        (Some(min), None) => {
            format!(
                "Must be {type_label} >= {}",
                format_num(min, is_integer_schema)
            )
        }
        (None, Some(max)) => {
            format!(
                "Must be {type_label} <= {}",
                format_num(max, is_integer_schema)
            )
        }
        (None, None) => format!("Must be {type_label}"),
    }
}

fn is_valid_email(value: &str) -> bool {
    static EMAIL: OnceLock<Regex> = OnceLock::new();
    EMAIL
        .get_or_init(|| Regex::new(r"^[^@\s]+@[^@\s]+\.[^@\s]+$").expect("email regex"))
        .is_match(value)
}

fn is_valid_uri(value: &str) -> bool {
    static URI: OnceLock<Regex> = OnceLock::new();
    URI.get_or_init(|| Regex::new(r"^[A-Za-z][A-Za-z0-9+.-]*://\S+$").expect("uri regex"))
        .is_match(value)
}

fn validate_string_format(value: &str, schema: &Value) -> Option<&'static str> {
    match schema.get("format").and_then(Value::as_str) {
        Some("email") if !is_valid_email(value) => {
            Some("Must be a valid email address, e.g. user@example.com")
        }
        Some("uri") if !is_valid_uri(value) => {
            Some("Must be a valid URI, e.g. https://example.com")
        }
        Some("date") if NaiveDate::parse_from_str(value, "%Y-%m-%d").is_err() => {
            Some("Must be a valid date, e.g. 2024-03-15, today, next Monday")
        }
        Some("date-time") if DateTime::parse_from_rfc3339(value).is_err() => {
            Some("Must be a valid date-time, e.g. 2024-03-15T14:30:00Z, tomorrow at 3pm")
        }
        _ => None,
    }
}

fn f64_value(value: f64) -> Value {
    Number::from_f64(value)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

/// Maps to: CC `validateElicitationInput(...)`.
pub fn validate_elicitation_input(string_value: &str, schema: &Value) -> ValidationResult {
    if is_enum_schema(schema) {
        if get_enum_values(schema)
            .iter()
            .any(|value| value == string_value)
        {
            return ValidationResult::valid(Value::String(string_value.to_string()));
        }
        return ValidationResult::invalid("Invalid enum value");
    }

    match schema_type(schema) {
        Some("string") => {
            let char_count = string_value.chars().count();
            if let Some(min) = schema.get("minLength").and_then(Value::as_u64)
                && char_count < min as usize
            {
                return ValidationResult::invalid(format!(
                    "Must be at least {min} {}",
                    plural(min as usize, "character")
                ));
            }
            if let Some(max) = schema.get("maxLength").and_then(Value::as_u64)
                && char_count > max as usize
            {
                return ValidationResult::invalid(format!(
                    "Must be at most {max} {}",
                    plural(max as usize, "character")
                ));
            }
            if let Some(error) = validate_string_format(string_value, schema) {
                return ValidationResult::invalid(error);
            }
            ValidationResult::valid(Value::String(string_value.to_string()))
        }
        Some("number") | Some("integer") => {
            let is_integer_schema = schema_type(schema) == Some("integer");
            let range_msg = number_range_message(schema, is_integer_schema);
            let Ok(parsed) = string_value.parse::<f64>() else {
                return ValidationResult::invalid(range_msg);
            };
            if is_integer_schema && parsed.fract() != 0.0 {
                return ValidationResult::invalid(range_msg);
            }
            if schema
                .get("minimum")
                .and_then(Value::as_f64)
                .is_some_and(|min| parsed < min)
                || schema
                    .get("maximum")
                    .and_then(Value::as_f64)
                    .is_some_and(|max| parsed > max)
            {
                return ValidationResult::invalid(range_msg);
            }
            if is_integer_schema {
                ValidationResult::valid(Value::Number(Number::from(parsed as i64)))
            } else {
                ValidationResult::valid(f64_value(parsed))
            }
        }
        Some("boolean") => {
            // Mirrors z.coerce.boolean(): JavaScript Boolean(stringValue).
            ValidationResult::valid(Value::Bool(!string_value.is_empty()))
        }
        _ => ValidationResult::invalid(format!("Unsupported schema: {schema}")),
    }
}

fn string_format(schema: &Value) -> Option<&str> {
    (schema_type(schema) == Some("string"))
        .then(|| schema.get("format").and_then(Value::as_str))
        .flatten()
}

/// Maps to: CC `getFormatHint(...)`.
pub fn get_format_hint(schema: &Value) -> Option<String> {
    match schema_type(schema) {
        Some("string") => match string_format(schema)? {
            "email" => Some("email address, e.g. user@example.com".to_string()),
            "uri" => Some("URI, e.g. https://example.com".to_string()),
            "date" => Some("date, e.g. 2024-03-15".to_string()),
            "date-time" => Some("date-time, e.g. 2024-03-15T14:30:00Z".to_string()),
            _ => Some("undefined, e.g. undefined".to_string()),
        },
        Some("number") | Some("integer") => {
            let is_integer_schema = schema_type(schema) == Some("integer");
            let minimum = schema.get("minimum").and_then(Value::as_f64);
            let maximum = schema.get("maximum").and_then(Value::as_f64);
            match (minimum, maximum) {
                (Some(min), Some(max)) => Some(format!(
                    "({} between {} and {})",
                    schema_type(schema)?,
                    format_num(min, is_integer_schema),
                    format_num(max, is_integer_schema)
                )),
                (Some(min), None) => Some(format!(
                    "({} >= {})",
                    schema_type(schema)?,
                    format_num(min, is_integer_schema)
                )),
                (None, Some(max)) => Some(format!(
                    "({} <= {})",
                    schema_type(schema)?,
                    format_num(max, is_integer_schema)
                )),
                (None, None) => Some(format!(
                    "({}, e.g. {})",
                    schema_type(schema)?,
                    if is_integer_schema { "42" } else { "3.14" }
                )),
            }
        }
        _ => None,
    }
}

/// Maps to: CC `isDateTimeSchema(...)`.
pub fn is_date_time_schema(schema: &Value) -> bool {
    schema_type(schema) == Some("string")
        && matches!(string_format(schema), Some("date" | "date-time"))
}

/// Maps to: CC `validateElicitationInputAsync(...)`.
pub async fn validate_elicitation_input_async(
    string_value: &str,
    schema: &Value,
) -> ValidationResult {
    validate_elicitation_input(string_value, schema)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_helpers_support_legacy_and_oneof_shapes() {
        let legacy = serde_json::json!({
            "type": "string",
            "enum": ["us", "uk"],
            "enumNames": ["United States", "United Kingdom"]
        });
        assert!(is_enum_schema(&legacy));
        assert_eq!(get_enum_values(&legacy), vec!["us", "uk"]);
        assert_eq!(
            get_enum_labels(&legacy),
            vec!["United States", "United Kingdom"]
        );
        assert_eq!(get_enum_label(&legacy, "uk"), "United Kingdom");
        assert_eq!(get_enum_label(&legacy, "ca"), "ca");

        let one_of = serde_json::json!({
            "type": "string",
            "oneOf": [
                {"const": "red", "title": "Red"},
                {"const": "blue", "title": "Blue"}
            ]
        });
        assert_eq!(get_enum_values(&one_of), vec!["red", "blue"]);
        assert_eq!(get_enum_label(&one_of, "red"), "Red");
    }

    #[test]
    fn multi_select_helpers_support_enum_and_anyof_shapes() {
        let enum_schema = serde_json::json!({
            "type": "array",
            "items": { "enum": ["a", "b"] }
        });
        assert!(is_multi_select_enum_schema(&enum_schema));
        assert_eq!(get_multi_select_values(&enum_schema), vec!["a", "b"]);
        assert_eq!(get_multi_select_label(&enum_schema, "b"), "b");

        let any_of = serde_json::json!({
            "type": "array",
            "items": {
                "anyOf": [
                    {"const": "low", "title": "Low"},
                    {"const": "high", "title": "High"}
                ]
            }
        });
        assert_eq!(get_multi_select_values(&any_of), vec!["low", "high"]);
        assert_eq!(get_multi_select_labels(&any_of), vec!["Low", "High"]);
        assert_eq!(get_multi_select_label(&any_of, "high"), "High");
    }

    #[test]
    fn validate_elicitation_input_matches_official_string_and_enum_errors() {
        let min = serde_json::json!({ "type": "string", "minLength": 3 });
        assert_eq!(
            validate_elicitation_input("ab", &min),
            ValidationResult::invalid("Must be at least 3 characters")
        );

        let max = serde_json::json!({ "type": "string", "maxLength": 1 });
        assert_eq!(
            validate_elicitation_input("ab", &max),
            ValidationResult::invalid("Must be at most 1 character")
        );

        let email = serde_json::json!({ "type": "string", "format": "email" });
        assert_eq!(
            validate_elicitation_input("not-an-email", &email)
                .error
                .as_deref(),
            Some("Must be a valid email address, e.g. user@example.com")
        );

        let enum_schema = serde_json::json!({ "type": "string", "enum": ["one"] });
        assert_eq!(
            validate_elicitation_input("two", &enum_schema),
            ValidationResult::invalid("Invalid enum value")
        );
    }

    #[test]
    fn validate_elicitation_input_matches_official_number_range_messages() {
        let number = serde_json::json!({ "type": "number", "minimum": 1, "maximum": 3 });
        assert_eq!(
            validate_elicitation_input("4", &number),
            ValidationResult::invalid("Must be a number between 1.0 and 3.0")
        );
        assert_eq!(
            validate_elicitation_input("2.5", &number),
            ValidationResult::valid(serde_json::json!(2.5))
        );

        let integer = serde_json::json!({ "type": "integer", "minimum": 1, "maximum": 3 });
        assert_eq!(
            validate_elicitation_input("2.5", &integer),
            ValidationResult::invalid("Must be an integer between 1 and 3")
        );
        assert_eq!(
            validate_elicitation_input("2", &integer),
            ValidationResult::valid(serde_json::json!(2))
        );
    }

    #[test]
    fn format_hints_and_date_time_detection_match_official_helpers() {
        assert_eq!(
            get_format_hint(&serde_json::json!({ "type": "string", "format": "uri" })),
            Some("URI, e.g. https://example.com".to_string())
        );
        assert_eq!(
            get_format_hint(&serde_json::json!({ "type": "integer" })),
            Some("(integer, e.g. 42)".to_string())
        );
        assert_eq!(
            get_format_hint(&serde_json::json!({ "type": "number", "minimum": 1 })),
            Some("(number >= 1.0)".to_string())
        );
        assert!(is_date_time_schema(
            &serde_json::json!({ "type": "string", "format": "date-time" })
        ));
        assert!(!is_date_time_schema(
            &serde_json::json!({ "type": "string", "format": "email" })
        ));
    }

    #[test]
    fn async_validation_is_safe_sync_fallback_until_natural_language_parser_is_ported() {
        let schema = serde_json::json!({ "type": "string", "format": "date" });
        let result =
            futures::executor::block_on(validate_elicitation_input_async("next Monday", &schema));
        assert_eq!(
            result,
            ValidationResult::invalid("Must be a valid date, e.g. 2024-03-15, today, next Monday")
        );
    }
}
