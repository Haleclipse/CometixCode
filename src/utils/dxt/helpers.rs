//! Maps to: CC `utils/dxt/helpers.ts`.
//! The dependency schema is the explicitly authorized MCPB 2.1.2
//! `dist/schemas/any.js` union (exported as `vAny.McpbManifestSchema`).
//! The rebuild's root-level import is incorrect; it is not reproduced as a
//! permanent validation failure. Schema adapters remain beside their consumer.

use crate::utils::zod::{self as z, Schema};
use serde_json::Value;
use std::sync::LazyLock;

/// Maps to: CC `utils/dxt/helpers.ts:13-35#validateManifest`.
pub async fn validate_manifest(manifest_json: &Value) -> anyhow::Result<Value> {
    static SCHEMA: LazyLock<Schema> = LazyLock::new(|| {
        z::union(
            ["0.1", "0.2", "0.3", "0.4"]
                .into_iter()
                .map(mcpb_manifest_schema)
                .collect(),
        )
    });
    let parsed = parse_manifest_schema(&SCHEMA, Some(manifest_json), None);
    if parsed.issues.is_empty() && !parsed.aborted {
        return Ok(parsed.data);
    }
    let mut fields = serde_json::Map::new();
    let mut forms = Vec::new();
    for (field, message) in parsed.issues {
        if let Some(field) = field {
            fields
                .entry(field)
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .unwrap()
                .push(Value::String(message));
        } else {
            forms.push(message);
        }
    }
    let mut messages: Vec<String> = crate::utils::process_env::ecmascript_object_entries(&fields)
        .into_iter()
        .map(|(field, errors)| {
            format!(
                "{field}: {}",
                errors
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
        .collect();
    messages.extend(forms);
    anyhow::bail!(
        "Invalid manifest: {}",
        messages
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("; ")
    )
}

/// Native dependency adapter for Zod 3's ParseStatus and the node kinds used by
/// MCPB. This intentionally does not change the shared Zod 4 interpreter: v3
/// unions return their first dirty branch, while aborted branches are hidden
/// behind a single form error. Only flatten()'s observable path/message survives.
#[derive(Default)]
struct ManifestParse {
    data: Value,
    aborted: bool,
    issues: Vec<(Option<String>, String)>,
}

fn parse_manifest_schema(
    schema: &Schema,
    input: Option<&Value>,
    field: Option<&str>,
) -> ManifestParse {
    let mut parsed = ManifestParse {
        data: input.cloned().unwrap_or(Value::Null),
        ..Default::default()
    };
    let issue = |message: String| (field.map(str::to_owned), message);
    match schema {
        Schema::Optional(inner) => {
            if input.is_none() {
                return parsed;
            }
            return parse_manifest_schema(inner, input, field);
        }
        Schema::Refine(predicate, message, inner) => {
            parsed = parse_manifest_schema(inner, input, field);
            if !parsed.aborted && !predicate(&parsed.data) {
                parsed.issues.push(issue((*message).into()));
            }
        }
        Schema::Union(branches) => {
            let mut dirty = None;
            for branch in branches {
                let branch = parse_manifest_schema(branch, input, field);
                if !branch.aborted && branch.issues.is_empty() {
                    return branch;
                }
                if !branch.aborted && dirty.is_none() {
                    dirty = Some(branch);
                }
            }
            if let Some(dirty) = dirty {
                return dirty;
            }
            parsed.aborted = true;
            parsed.issues.push(issue("Invalid input".into()));
        }
        Schema::StrictObject(shape) => {
            let Some(object) = input.and_then(Value::as_object) else {
                parsed.aborted = true;
                return parsed;
            };
            let mut output = serde_json::Map::new();
            for (name, child) in shape {
                let result = parse_manifest_schema(child, object.get(*name), field.or(Some(*name)));
                parsed.aborted |= result.aborted;
                parsed.issues.extend(result.issues);
                if object.contains_key(*name) {
                    output.insert((*name).into(), result.data);
                }
            }
            let extra: Vec<_> = crate::utils::process_env::ecmascript_object_entries(object)
                .into_iter()
                .map(|(key, _)| key)
                .filter(|key| !shape.iter().any(|(name, _)| name == key))
                .collect();
            if !extra.is_empty() {
                parsed.issues.push(issue(format!(
                    "Unrecognized key(s) in object: {}",
                    extra
                        .iter()
                        .map(|key| format!("'{key}'"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
            parsed.data = Value::Object(output);
        }
        Schema::Record(child) => {
            let Some(object) = input.and_then(Value::as_object) else {
                parsed.aborted = true;
                return parsed;
            };
            let mut output = serde_json::Map::new();
            for (name, value) in crate::utils::process_env::ecmascript_object_entries(object) {
                // Zod3 parses all record values, but mergeObjectSync excludes
                // __proto__ only when assigning the validated result.
                let result = parse_manifest_schema(child, Some(value), field.or(Some(name)));
                parsed.aborted |= result.aborted;
                parsed.issues.extend(result.issues);
                if name != "__proto__" {
                    output.insert(name.into(), result.data);
                }
            }
            parsed.data = Value::Object(output);
        }
        Schema::Array { items, .. } => {
            let Some(array) = input.and_then(Value::as_array) else {
                parsed.aborted = true;
                return parsed;
            };
            let mut output = Vec::new();
            for value in array {
                let result = parse_manifest_schema(items, Some(value), field);
                parsed.aborted |= result.aborted;
                parsed.issues.extend(result.issues);
                output.push(result.data);
            }
            parsed.data = Value::Array(output);
        }
        Schema::String { .. } => {
            let Some(value) = input.filter(|v| v.is_string()) else {
                parsed.aborted = true;
                return parsed;
            };
            if let Err(errors) = z::safe_parse(schema, value) {
                parsed.issues.extend(errors.issues.into_iter().map(|error| {
                    issue(if error.message == "Invalid URL" {
                        "Invalid url".into()
                    } else {
                        error.message
                    })
                }));
            }
        }
        Schema::Number { .. } => parsed.aborted = !input.is_some_and(Value::is_number),
        Schema::Boolean => parsed.aborted = !input.is_some_and(Value::is_boolean),
        Schema::Literal(value) => parsed.aborted = input != Some(value),
        Schema::Enum(values) => {
            parsed.aborted = !input
                .and_then(Value::as_str)
                .is_some_and(|s| values.contains(&s))
        }
        Schema::Any => (),
        _ => unreachable!("MCPB uses only the schema nodes declared in this owner"),
    }
    parsed
}

/// Maps to: MCPB `schemas/0.1.js`–`0.4.js#McpServerConfigSchema`.
fn mcp_server_config_schema() -> Schema {
    z::strict_object(vec![
        ("command", z::string()),
        ("args", z::array(z::string()).optional()),
        ("env", z::record(z::string()).optional()),
    ])
}

/// Maps to: MCPB `schemas/0.1.js`–`0.4.js#McpbManifestPlatformOverrideSchema`.
fn mcpb_manifest_platform_override_schema() -> Schema {
    let Schema::StrictObject(fields) = mcp_server_config_schema() else {
        unreachable!()
    };
    z::strict_object(
        fields
            .into_iter()
            .map(|(name, schema)| (name, schema.optional()))
            .collect(),
    )
}

/// Maps to: MCPB's `z.string().email()` via Zod 3 `types.js#emailRegex`.
/// Only the consumer's flattened message is exposed, so this private refine
/// preserves both string type rejection and the original email predicate.
fn manifest_email_schema() -> Schema {
    z::string().refine(|value| {
        static EMAIL: LazyLock<regress::Regex> = LazyLock::new(|| regress::Regex::new(
            r"^(?!\.)(?!.*\.\.)([A-Za-z0-9_'+\-\.]*)[A-Za-z0-9_+-]@([A-Za-z0-9][A-Za-z0-9\-]*\.)+[A-Za-z]{2,}$"
        ).expect("MCPB Zod email expression"));
        EMAIL.find(value.as_str().unwrap()).is_some()
    }, "Invalid email")
}

/// Maps to: MCPB `schemas/0.1.js`–`0.4.js#McpbManifestAuthorSchema`.
fn mcpb_manifest_author_schema() -> Schema {
    z::strict_object(vec![
        ("name", z::string()),
        ("email", manifest_email_schema().optional()),
        ("url", z::string().url().optional()),
    ])
}

/// Maps to: MCPB `schemas/0.1.js`–`0.4.js#McpbManifestRepositorySchema`.
fn mcpb_manifest_repository_schema() -> Schema {
    z::strict_object(vec![("type", z::string()), ("url", z::string().url())])
}

/// Maps to: MCPB `schemas/0.1.js`–`0.4.js#McpbManifestMcpConfigSchema`.
fn mcpb_manifest_mcp_config_schema() -> Schema {
    let Schema::StrictObject(mut fields) = mcp_server_config_schema() else {
        unreachable!()
    };
    fields.push((
        "platform_overrides",
        z::record(mcpb_manifest_platform_override_schema()).optional(),
    ));
    z::strict_object(fields)
}

/// Maps to: MCPB `schemas/0.1.js`–`0.4.js#McpbManifestServerSchema`.
fn mcpb_manifest_server_schema(version: &str) -> Schema {
    let mut types = vec!["python", "node", "binary"];
    if version == "0.4" {
        types.push("uv");
    }
    z::strict_object(vec![
        ("type", z::enumeration(types)),
        ("entry_point", z::string()),
        ("mcp_config", mcpb_manifest_mcp_config_schema()),
    ])
}

/// Maps to: MCPB `schemas/0.1.js`–`0.4.js#McpbManifestCompatibilitySchema`.
fn mcpb_manifest_compatibility_schema() -> Schema {
    z::strict_object(vec![
        ("claude_desktop", z::string().optional()),
        (
            "platforms",
            z::array(z::enumeration(vec!["darwin", "win32", "linux"])).optional(),
        ),
        (
            "runtimes",
            z::strict_object(vec![
                ("python", z::string().optional()),
                ("node", z::string().optional()),
            ])
            .optional(),
        ),
    ])
}

/// Maps to: MCPB `schemas/0.1.js`–`0.4.js#McpbManifestToolSchema`.
fn mcpb_manifest_tool_schema() -> Schema {
    z::strict_object(vec![
        ("name", z::string()),
        ("description", z::string().optional()),
    ])
}

/// Maps to: MCPB `schemas/0.1.js`–`0.4.js#McpbManifestPromptSchema`.
fn mcpb_manifest_prompt_schema() -> Schema {
    z::strict_object(vec![
        ("name", z::string()),
        ("description", z::string().optional()),
        ("arguments", z::array(z::string()).optional()),
        ("text", z::string()),
    ])
}

/// Maps to: MCPB `schemas/0.1.js`–`0.4.js#McpbUserConfigurationOptionSchema`.
fn mcpb_user_configuration_option_schema() -> Schema {
    z::strict_object(vec![
        (
            "type",
            z::enumeration(vec!["string", "number", "boolean", "directory", "file"]),
        ),
        ("title", z::string()),
        ("description", z::string()),
        ("required", z::boolean().optional()),
        (
            "default",
            z::union(vec![
                z::string(),
                z::number(),
                z::boolean(),
                z::array(z::string()),
            ])
            .optional(),
        ),
        ("multiple", z::boolean().optional()),
        ("sensitive", z::boolean().optional()),
        ("min", z::number().optional()),
        ("max", z::number().optional()),
    ])
}

/// Maps to: MCPB `schemas/0.3.js`–`0.4.js#McpbManifestLocalizationSchema`.
fn mcpb_manifest_localization_schema() -> Schema {
    z::strict_object(vec![
        (
            "resources",
            z::string().regex_with_flags_and_message(
                r"\$\{locale\}",
                "i",
                "resources must include a \"${locale}\" placeholder",
            ),
        ),
        (
            "default_locale",
            z::string().regex_with_message(
                r"^[A-Za-z0-9]{2,8}(?:-[A-Za-z0-9]{1,8})*$",
                "default_locale must be a valid BCP 47 locale identifier",
            ),
        ),
    ])
}

/// Maps to: MCPB `schemas/0.3.js`–`0.4.js#McpbManifestIconSchema`.
fn mcpb_manifest_icon_schema() -> Schema {
    z::strict_object(vec![
        ("src", z::string()),
        (
            "size",
            z::string().regex_with_message(
                r"^[0-9]+x[0-9]+$",
                "size must be in the format \"WIDTHxHEIGHT\" (e.g., \"16x16\")",
            ),
        ),
        (
            "theme",
            z::string()
                .min_with_message(1, "theme cannot be empty when provided")
                .optional(),
        ),
    ])
}

/// Maps to: MCPB `schemas/0.1.js`–`0.4.js#McpbManifestSchema`.
/// A version parameter shares identical declarations without merging the union
/// branches or admitting fields that earlier schemas reject.
fn mcpb_manifest_schema(version: &'static str) -> Schema {
    let mut fields = vec![
        ("$schema", z::string().optional()),
        (
            "dxt_version",
            z::literal(Value::String(version.into())).optional(),
        ),
        (
            "manifest_version",
            z::literal(Value::String(version.into())).optional(),
        ),
        ("name", z::string()),
        ("display_name", z::string().optional()),
        ("version", z::string()),
        ("description", z::string()),
        ("long_description", z::string().optional()),
        ("author", mcpb_manifest_author_schema()),
        ("repository", mcpb_manifest_repository_schema().optional()),
        ("homepage", z::string().url().optional()),
        ("documentation", z::string().url().optional()),
        ("support", z::string().url().optional()),
        ("icon", z::string().optional()),
    ];
    if version == "0.3" || version == "0.4" {
        fields.push(("icons", z::array(mcpb_manifest_icon_schema()).optional()));
    }
    fields.push(("screenshots", z::array(z::string()).optional()));
    if version == "0.3" || version == "0.4" {
        fields.push((
            "localization",
            mcpb_manifest_localization_schema().optional(),
        ));
    }
    fields.extend([
        ("server", mcpb_manifest_server_schema(version)),
        ("tools", z::array(mcpb_manifest_tool_schema()).optional()),
        ("tools_generated", z::boolean().optional()),
        (
            "prompts",
            z::array(mcpb_manifest_prompt_schema()).optional(),
        ),
        ("prompts_generated", z::boolean().optional()),
        ("keywords", z::array(z::string()).optional()),
        ("license", z::string().optional()),
    ]);
    if version != "0.1" {
        fields.push(("privacy_policies", z::array(z::string().url()).optional()));
    }
    fields.extend([
        (
            "compatibility",
            mcpb_manifest_compatibility_schema().optional(),
        ),
        (
            "user_config",
            z::record(mcpb_user_configuration_option_schema()).optional(),
        ),
    ]);
    if version == "0.3" || version == "0.4" {
        fields.push(("_meta", z::record(z::record(z::any())).optional()));
    }
    z::strict_object(fields).refine(
        |data| data.get("dxt_version").is_some() || data.get("manifest_version").is_some(),
        "Either 'dxt_version' (deprecated) or 'manifest_version' must be provided",
    )
}

/// Maps to: CC `utils/dxt/helpers.ts:40-51#parseAndValidateManifestFromText`.
pub async fn parse_and_validate_manifest_from_text(manifest_text: &str) -> anyhow::Result<Value> {
    let manifest_json = crate::utils::slow_operations::json_parse(manifest_text)
        .map_err(|error| anyhow::anyhow!("Invalid JSON in manifest.json: {error}"))?;
    validate_manifest(&manifest_json.to_json()).await
}

/// Maps to: CC `utils/dxt/helpers.ts:56-62#parseAndValidateManifestFromBytes`.
pub async fn parse_and_validate_manifest_from_bytes(manifest_data: &[u8]) -> anyhow::Result<Value> {
    // TextDecoder's default UTF-8 replacement decoding removes one initial BOM.
    let text = String::from_utf8_lossy(manifest_data);
    parse_and_validate_manifest_from_text(text.strip_prefix('\u{feff}').unwrap_or(&text)).await
}

/// Maps to: CC `utils/dxt/helpers.ts#generateExtensionId`.
pub fn generate_extension_id(manifest: &Value, prefix: Option<&str>) -> String {
    fn sanitize(value: &str) -> String {
        // JS \s is ECMAScript whitespace, including FEFF but excluding NEL.
        fn whitespace(c: char) -> bool {
            matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
        }
        let mut output = String::new();
        for c in value.to_lowercase().chars() {
            let c = if whitespace(c) { '-' } else { c };
            if (c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
                && (c != '-' || !output.ends_with('-'))
            {
                output.push(c);
            }
        }
        output.trim_matches('-').to_owned()
    }
    let author = sanitize(
        manifest["author"]["name"]
            .as_str()
            .expect("validated manifest author name"),
    );
    let name = sanitize(manifest["name"].as_str().expect("validated manifest name"));
    match prefix.filter(|p| !p.is_empty()) {
        Some(prefix) => format!("{prefix}.{author}.{name}"),
        None => format!("{author}.{name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mcpb_manifest_matches_published_v_any_bun_oracle() {
        let oracle: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/oracles/mcpb-schema-0915/oracle.json"
        ))
        .unwrap();
        let mut differences = Vec::new();
        for case in oracle["cases"].as_array().unwrap() {
            let actual = match validate_manifest(&case["input"]).await {
                Ok(data) => serde_json::json!({"ok": true, "data": data}),
                Err(error) => serde_json::json!({"ok": false, "error": error.to_string()}),
            };
            if actual != case["result"] {
                differences.push(format!(
                    "{}: actual={actual}; expected={}",
                    case["name"], case["result"]
                ));
            }
        }
        assert!(differences.is_empty(), "{}", differences.join("\n"));
    }

    #[tokio::test]
    async fn mcpb_text_decoder_and_json_error_boundary() {
        let text = r#"{"manifest_version":"0.1","name":"n","version":"1","description":"d","author":{"name":"a"},"server":{"type":"node","entry_point":"x","mcp_config":{"command":"node"}}}"#;
        let mut bytes = b"\xef\xbb\xbf".to_vec();
        bytes.extend_from_slice(text.as_bytes());
        assert_eq!(
            parse_and_validate_manifest_from_bytes(&bytes)
                .await
                .unwrap(),
            serde_json::from_str::<Value>(text).unwrap()
        );
        assert!(
            parse_and_validate_manifest_from_text("{")
                .await
                .unwrap_err()
                .to_string()
                .starts_with("Invalid JSON in manifest.json: ")
        );
    }

    #[test]
    fn extension_id_uses_source_sanitization() {
        let manifest = serde_json::json!({"author":{"name":"  A\u{feff}B / C -- "}, "name":" Hello__World.v2! "});
        assert_eq!(
            generate_extension_id(&manifest, None),
            "a-b-c.hello__world.v2"
        );
        assert_eq!(
            generate_extension_id(&manifest, Some("local.dxt")),
            "local.dxt.a-b-c.hello__world.v2"
        );
    }
}
