//! Maps to: CC `utils/plugins/validatePlugin.ts`.
//! Developer validation owns stricter manifest schemas, diagnostics, and the
//! default-layout content scan. It does not install, load, or mutate plugins.

use super::schemas::{
    plugin_hooks_schema, plugin_manifest_schema, plugin_marketplace_entry_schema,
    plugin_marketplace_schema,
};
use crate::utils::{
    errors::{format_native_file_error, io_errno_code},
    frontmatter_parser::FRONTMATTER_REGEX,
    slow_operations::json_parse,
    yaml::parse_yaml,
    zod::{self, PathSegment, ZodError},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Component, Path, PathBuf};
use tokio::fs;

/// Maps to: CC `validatePlugin.ts:24-30#MARKETPLACE_ONLY_MANIFEST_FIELDS`.
const MARKETPLACE_ONLY_MANIFEST_FIELDS: &[&str] = &["category", "source", "tags", "strict", "id"];

/// Maps to: CC `validatePlugin.ts:32-39#ValidationResult` fileType union.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ValidationFileType {
    Plugin,
    Marketplace,
    Skill,
    Agent,
    Command,
    Hooks,
}
impl ValidationFileType {
    /// String-literal projection of the source union for diagnostic interpolation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plugin => "plugin",
            Self::Marketplace => "marketplace",
            Self::Skill => "skill",
            Self::Agent => "agent",
            Self::Command => "command",
            Self::Hooks => "hooks",
        }
    }
}
/// Maps to: CC `validatePlugin.ts:32-39#ValidationResult`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ValidationResult {
    pub success: bool,
    pub errors: Vec<ValidationError>,
    pub warnings: Vec<ValidationWarning>,
    pub file_path: String,
    pub file_type: ValidationFileType,
}
/// Maps to: CC `validatePlugin.ts:41-45#ValidationError`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidationError {
    pub path: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}
/// Maps to: CC `validatePlugin.ts:47-50#ValidationWarning`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidationWarning {
    pub path: String,
    pub message: String,
}

// L1: Node path.resolve primitive, not utils/path.ts normalization (which
// also trims, expands ~, and applies NFC). Does not resolve symlinks.
fn resolve_path(path: &str) -> std::io::Result<PathBuf> {
    let input = Path::new(path);
    let absolute = if input.is_absolute() {
        input.to_owned()
    } else {
        std::env::current_dir()?.join(input)
    };
    let mut result = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            part => result.push(part.as_os_str()),
        }
    }
    Ok(result)
}
// L1: JS truthiness of the existing JSON carrier, used only where the source
// explicitly branches on a parsed property (empty arrays/objects are true).
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(v)) => *v,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|v| v != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(_) | Value::Object(_)) => true,
    }
}

/// Maps to: CC `validatePlugin.ts:54-70#detectManifestType`.
fn detect_manifest_type(file_path: &str) -> Option<ValidationFileType> {
    let path = Path::new(file_path);
    match path.file_name().and_then(|s| s.to_str()) {
        Some("plugin.json") => Some(ValidationFileType::Plugin),
        Some("marketplace.json") => Some(ValidationFileType::Marketplace),
        _ => (path
            .parent()
            .and_then(Path::file_name)
            .and_then(|s| s.to_str())
            == Some(".claude-plugin"))
        .then_some(ValidationFileType::Plugin),
    }
}
/// Maps to: CC `validatePlugin.ts:75-81#formatZodErrors`.
fn format_zod_errors(error: ZodError) -> Vec<ValidationError> {
    error
        .issues
        .into_iter()
        .map(|issue| {
            let path = issue
                .path
                .iter()
                .map(|p| match p {
                    PathSegment::Key(k) => k.clone(),
                    PathSegment::Index(i) => i.to_string(),
                })
                .collect::<Vec<_>>()
                .join(".");
            ValidationError {
                path: if path.is_empty() { "root".into() } else { path },
                message: issue.message,
                code: Some(issue.code.as_str().into()),
            }
        })
        .collect()
}
/// Maps to: CC `validatePlugin.ts:92-106#checkPathTraversal`.
fn check_path_traversal(
    p: &str,
    field: &str,
    errors: &mut Vec<ValidationError>,
    hint: Option<&str>,
) {
    if p.contains("..") {
        errors.push(ValidationError {
            path: field.into(),
            message: match hint {
                Some(hint) => format!("Path contains \"..\": {p}. {hint}"),
                None => {
                    format!("Path contains \"..\" which could be a path traversal attempt: {p}")
                }
            },
            code: None,
        });
    }
}
/// Maps to: CC `validatePlugin.ts:113-124#marketplaceSourceHint`.
fn marketplace_source_hint(p: &str) -> String {
    let stripped = p.trim_start_matches("../");
    let corrected = if stripped != p {
        format!("./{stripped}")
    } else {
        "./plugins/my-plugin".into()
    };
    format!(
        "Plugin source paths are resolved relative to the marketplace root (the directory containing .claude-plugin/), not relative to marketplace.json. Use \"{corrected}\" instead of \"{p}\"."
    )
}

/// Maps to: CC `validatePlugin.ts:129-305#validatePluginManifest`.
/// Fallibility transports Node path.resolve's process-cwd failure; ordinary
/// file/parse/schema errors remain ValidationResult entries, as in CC.
pub async fn validate_plugin_manifest(file_path: &str) -> anyhow::Result<ValidationResult> {
    let absolute_path = resolve_path(file_path)?;
    let file_path = absolute_path.to_string_lossy().into_owned();
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let content = match fs::read(&absolute_path).await {
        Ok(content) => String::from_utf8_lossy(&content).into_owned(),
        Err(error) => {
            let code = io_errno_code(&error);
            let message = match code {
                Some("ENOENT") => format!("File not found: {file_path}"),
                Some("EISDIR") => format!("Path is not a file: {file_path}"),
                _ => format!(
                    "Failed to read file: {}",
                    format_native_file_error(&error, "open", Some(&absolute_path))
                ),
            };
            return Ok(ValidationResult {
                success: false,
                errors: vec![ValidationError {
                    path: "file".into(),
                    message,
                    code: code.map(str::to_owned),
                }],
                warnings: vec![],
                file_path,
                file_type: ValidationFileType::Plugin,
            });
        }
    };
    let parsed = match json_parse(&content) {
        Ok(parsed) => parsed.to_json(),
        Err(error) => {
            return Ok(ValidationResult {
                success: false,
                errors: vec![ValidationError {
                    path: "json".into(),
                    message: format!("Invalid JSON syntax: {error}"),
                    code: None,
                }],
                warnings: vec![],
                file_path,
                file_type: ValidationFileType::Plugin,
            });
        }
    };
    if let Some(obj) = parsed.as_object() {
        for field in ["commands", "agents", "skills"] {
            if truthy(obj.get(field))
                && let Some(value) = obj.get(field)
            {
                let values = value
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or(std::slice::from_ref(value));
                for (i, value) in values.iter().enumerate() {
                    if let Some(p) = value.as_str() {
                        check_path_traversal(p, &format!("{field}[{i}]"), &mut errors, None);
                    }
                }
            }
        }
    }
    let mut to_validate = parsed;
    if let Some(obj) = to_validate.as_object_mut() {
        // Preserve Object.keys insertion order; do not iterate the Set instead.
        let stray_keys = obj
            .keys()
            .filter(|k| MARKETPLACE_ONLY_MANIFEST_FIELDS.contains(&k.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        for key in stray_keys {
            obj.shift_remove(&key);
            warnings.push(ValidationWarning{path:key.clone(),message:format!("Field '{key}' belongs in the marketplace entry (marketplace.json), not plugin.json. It's harmless here but unused — nyan ignores it at load time.")});
        }
    }
    match zod::safe_parse(&plugin_manifest_schema().clone().strict(), &to_validate) {
        Err(error) => errors.extend(format_zod_errors(error)),
        Ok(manifest) => {
            let name = manifest["name"]
                .as_str()
                .expect("schema supplies string name");
            if !regex::Regex::new(r"^[a-z0-9]+(-[a-z0-9]+)*$")
                .expect("source regex")
                .is_match(name)
            {
                warnings.push(ValidationWarning{path:"name".into(),message:format!("Plugin name \"{name}\" is not kebab-case. nyan accepts it, but the Claude.ai marketplace sync requires kebab-case (lowercase letters, digits, and hyphens only, e.g., \"my-plugin\").")});
            }
            for (field, message) in [
                (
                    "version",
                    "No version specified. Consider adding a version following semver (e.g., \"1.0.0\")",
                ),
                (
                    "description",
                    "No description provided. Adding a description helps users understand what your plugin does",
                ),
                (
                    "author",
                    "No author information provided. Consider adding author details for plugin attribution",
                ),
            ] {
                if !truthy(manifest.get(field)) {
                    warnings.push(ValidationWarning {
                        path: field.into(),
                        message: message.into(),
                    });
                }
            }
        }
    }
    Ok(ValidationResult {
        success: errors.is_empty(),
        errors,
        warnings,
        file_path,
        file_type: ValidationFileType::Plugin,
    })
}

/// Maps to: CC `validatePlugin.ts:310-507#validateMarketplaceManifest`.
pub async fn validate_marketplace_manifest(file_path: &str) -> anyhow::Result<ValidationResult> {
    let absolute_path = resolve_path(file_path)?;
    let file_path = absolute_path.to_string_lossy().into_owned();
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let content = match fs::read(&absolute_path).await {
        Ok(content) => String::from_utf8_lossy(&content).into_owned(),
        Err(error) => {
            let code = io_errno_code(&error);
            let message = match code {
                Some("ENOENT") => format!("File not found: {file_path}"),
                Some("EISDIR") => format!("Path is not a file: {file_path}"),
                _ => format!(
                    "Failed to read file: {}",
                    format_native_file_error(&error, "open", Some(&absolute_path))
                ),
            };
            return Ok(ValidationResult {
                success: false,
                errors: vec![ValidationError {
                    path: "file".into(),
                    message,
                    code: code.map(str::to_owned),
                }],
                warnings: vec![],
                file_path,
                file_type: ValidationFileType::Marketplace,
            });
        }
    };
    let parsed = match json_parse(&content) {
        Ok(parsed) => parsed.to_json(),
        Err(error) => {
            return Ok(ValidationResult {
                success: false,
                errors: vec![ValidationError {
                    path: "json".into(),
                    message: format!("Invalid JSON syntax: {error}"),
                    code: None,
                }],
                warnings: vec![],
                file_path,
                file_type: ValidationFileType::Marketplace,
            });
        }
    };
    if let Some(plugins) = parsed.get("plugins").and_then(Value::as_array) {
        for (i, plugin) in plugins.iter().enumerate() {
            if let Some(source) = plugin.get("source") {
                if let Some(source) = source.as_str() {
                    check_path_traversal(
                        source,
                        &format!("plugins[{i}].source"),
                        &mut errors,
                        Some(&marketplace_source_hint(source)),
                    );
                }
                if let Some(p) = source.get("path").and_then(Value::as_str) {
                    check_path_traversal(
                        p,
                        &format!("plugins[{i}].source.path"),
                        &mut errors,
                        None,
                    );
                }
            }
        }
    }
    // Maps to: validatePluginMarketplace's .extend({ plugins }).strict().
    // L1: compose the canonical object fields at the defining call site.
    let zod::Schema::Object(mut fields) = plugin_marketplace_schema().clone() else {
        unreachable!("PluginMarketplaceSchema is an object")
    };
    for (name, field) in &mut fields {
        if *name == "plugins" {
            *field = zod::array(plugin_marketplace_entry_schema().clone().strict());
        }
    }
    let strict_schema = zod::strict_object(fields);
    match zod::safe_parse(&strict_schema, &parsed) {
        Err(error) => errors.extend(format_zod_errors(error)),
        Ok(marketplace) => {
            let plugins = marketplace["plugins"]
                .as_array()
                .expect("schema supplies plugins array");
            if plugins.is_empty() {
                warnings.push(ValidationWarning {
                    path: "plugins".into(),
                    message: "Marketplace has no plugins defined".into(),
                });
            }
            for (i, plugin) in plugins.iter().enumerate() {
                if plugins
                    .iter()
                    .filter(|p| p["name"] == plugin["name"])
                    .count()
                    > 1
                {
                    errors.push(ValidationError {
                        path: format!("plugins[{i}].name"),
                        message: format!(
                            "Duplicate plugin name \"{}\" found in marketplace",
                            plugin["name"].as_str().expect("schema name")
                        ),
                        code: None,
                    });
                }
            }
            let manifest_dir = absolute_path.parent().expect("absolute manifest parent");
            let marketplace_root = if manifest_dir
                .file_name()
                .is_some_and(|n| n == ".claude-plugin")
            {
                manifest_dir.parent().unwrap_or(manifest_dir)
            } else {
                manifest_dir
            };
            for (i, entry) in plugins.iter().enumerate() {
                let Some(version) = entry
                    .get("version")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                else {
                    continue;
                };
                let Some(source) = entry["source"].as_str().filter(|s| s.starts_with("./")) else {
                    continue;
                };
                let plugin_json_path = marketplace_root
                    .join(source)
                    .join(".claude-plugin/plugin.json");
                let Ok(raw) = fs::read(plugin_json_path).await else {
                    continue;
                };
                let Ok(parsed) = json_parse(&String::from_utf8_lossy(&raw)) else {
                    continue;
                };
                let Some(manifest_version) = parsed
                    .get_property("version")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                else {
                    continue;
                };
                if manifest_version != version {
                    warnings.push(ValidationWarning{path:format!("plugins[{i}].version"),message:format!("Entry declares version \"{version}\" but {source}/.claude-plugin/plugin.json says \"{manifest_version}\". At install time, plugin.json wins (calculatePluginVersion precedence) — the entry version is silently ignored. Update this entry to \"{manifest_version}\" to match.")});
                }
            }
            if !truthy(
                marketplace
                    .get("metadata")
                    .and_then(|v| v.get("description")),
            ) {
                warnings.push(ValidationWarning{path:"metadata.description".into(),message:"No marketplace description provided. Adding a description helps users understand what this marketplace offers".into()});
            }
        }
    }
    Ok(ValidationResult {
        success: errors.is_empty(),
        errors,
        warnings,
        file_path,
        file_type: ValidationFileType::Marketplace,
    })
}

/// Maps to: CC `validatePlugin.ts:517-639#validateComponentFile`.
fn validate_component_file(
    file_path: &str,
    content: &str,
    file_type: ValidationFileType,
) -> ValidationResult {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let Some(captures) = FRONTMATTER_REGEX.captures(content) else {
        warnings.push(ValidationWarning{path:"frontmatter".into(),message:"No frontmatter block found. Add YAML frontmatter between --- delimiters at the top of the file to set description and other metadata.".into()});
        return ValidationResult {
            success: true,
            errors,
            warnings,
            file_path: file_path.into(),
            file_type,
        };
    };
    let parsed = match parse_yaml(captures.get(1).map_or("", |m| m.as_str())) {
        Ok(parsed) => parsed,
        Err(error) => {
            errors.push(ValidationError{path:"frontmatter".into(),message:format!("YAML frontmatter failed to parse: {error}. At runtime this {} loads with empty metadata (all frontmatter fields silently dropped).",file_type.as_str()),code:None});
            return ValidationResult {
                success: false,
                errors,
                warnings,
                file_path: file_path.into(),
                file_type,
            };
        }
    };
    let Some(fm) = parsed.as_mapping() else {
        let kind = if parsed.is_sequence() {
            "an array"
        } else if parsed.is_null() {
            "null"
        } else {
            yaml_typeof(&parsed)
        };
        errors.push(ValidationError {
            path: "frontmatter".into(),
            message: format!("Frontmatter must be a YAML mapping (key: value pairs), got {kind}."),
            code: None,
        });
        return ValidationResult {
            success: false,
            errors,
            warnings,
            file_path: file_path.into(),
            file_type,
        };
    };
    if let Some(d) = fm.get("description") {
        if !d.is_string() && !d.is_number() && !d.is_bool() && !d.is_null() {
            let kind = if d.is_sequence() {
                "array"
            } else {
                yaml_typeof(d)
            };
            errors.push(ValidationError {
                path: "description".into(),
                message: format!(
                    "description must be a string, got {kind}. At runtime this value is dropped."
                ),
                code: None,
            });
        }
    } else {
        warnings.push(ValidationWarning{path:"description".into(),message:format!("No description in frontmatter. A description helps users and Claude understand when to use this {}.",file_type.as_str())});
    }
    if let Some(name) = fm.get("name").filter(|v| !v.is_null() && !v.is_string()) {
        errors.push(ValidationError {
            path: "name".into(),
            message: format!("name must be a string, got {}.", yaml_typeof(name)),
            code: None,
        });
    }
    if let Some(at) = fm.get("allowed-tools").filter(|v| !v.is_null()) {
        if !at.is_string() && !at.is_sequence() {
            errors.push(ValidationError {
                path: "allowed-tools".into(),
                message: format!(
                    "allowed-tools must be a string or array of strings, got {}.",
                    yaml_typeof(at)
                ),
                code: None,
            });
        } else if at
            .as_sequence()
            .is_some_and(|items| items.iter().any(|v| !v.is_string()))
        {
            errors.push(ValidationError {
                path: "allowed-tools".into(),
                message: "allowed-tools array must contain only strings.".into(),
                code: None,
            });
        }
    }
    if let Some(sh) = fm.get("shell").filter(|v| !v.is_null()) {
        if let Some(sh) = sh.as_str() {
            let normalized = sh
                .trim_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}')
                .to_lowercase();
            if normalized != "bash" && normalized != "powershell" {
                errors.push(ValidationError {
                    path: "shell".into(),
                    message: format!("shell must be 'bash' or 'powershell', got '{sh}'."),
                    code: None,
                });
            }
        } else {
            errors.push(ValidationError {
                path: "shell".into(),
                message: format!("shell must be a string, got {}.", yaml_typeof(sh)),
                code: None,
            });
        }
    }
    ValidationResult {
        success: errors.is_empty(),
        errors,
        warnings,
        file_path: file_path.into(),
        file_type,
    }
}

// L1: JS `typeof` for the native YAML value carrier. Arrays are objects under
// typeof; source branches which say 'array' perform a separate array check.
fn yaml_typeof(value: &serde_yaml::Value) -> &'static str {
    match value {
        serde_yaml::Value::Null
        | serde_yaml::Value::Sequence(_)
        | serde_yaml::Value::Mapping(_) => "object",
        serde_yaml::Value::Bool(_) => "boolean",
        serde_yaml::Value::Number(_) => "number",
        serde_yaml::Value::String(_) => "string",
        serde_yaml::Value::Tagged(value) => yaml_typeof(&value.value),
    }
}

/// Maps to: CC `validatePlugin.ts:646-711#validateHooksJson`.
async fn validate_hooks_json(file_path: &str) -> ValidationResult {
    let content = match fs::read(file_path).await {
        Ok(content) => String::from_utf8_lossy(&content).into_owned(),
        Err(error) => {
            let errors = if io_errno_code(&error) == Some("ENOENT") {
                vec![]
            } else {
                vec![ValidationError {
                    path: "file".into(),
                    message: format!(
                        "Failed to read file: {}",
                        format_native_file_error(&error, "open", Some(Path::new(file_path)))
                    ),
                    code: None,
                }]
            };
            return ValidationResult {
                success: errors.is_empty(),
                errors,
                warnings: vec![],
                file_path: file_path.into(),
                file_type: ValidationFileType::Hooks,
            };
        }
    };
    let parsed = match json_parse(&content) {
        Ok(parsed) => parsed.to_json(),
        Err(error) => {
            return ValidationResult {
                success: false,
                errors: vec![ValidationError {
                    path: "json".into(),
                    message: format!(
                        "Invalid JSON syntax: {error}. At runtime this breaks the entire plugin load."
                    ),
                    code: None,
                }],
                warnings: vec![],
                file_path: file_path.into(),
                file_type: ValidationFileType::Hooks,
            };
        }
    };
    let errors = zod::safe_parse(plugin_hooks_schema(), &parsed)
        .err()
        .map(format_zod_errors)
        .unwrap_or_default();
    ValidationResult {
        success: errors.is_empty(),
        errors,
        warnings: vec![],
        file_path: file_path.into(),
        file_type: ValidationFileType::Hooks,
    }
}

/// Maps to: CC `validatePlugin.ts:718-752#collectMarkdown`.
/// Box::pin only gives the recursive async future a finite Rust size.
async fn collect_markdown(dir: &Path, is_skills_dir: bool) -> std::io::Result<Vec<PathBuf>> {
    let mut entries = match fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(error) if matches!(io_errno_code(&error), Some("ENOENT" | "ENOTDIR")) => {
            return Ok(vec![]);
        }
        Err(error) => return Err(error),
    };
    let mut out = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let file_type = entry.file_type().await?;
        if is_skills_dir {
            if file_type.is_dir() {
                out.push(entry.path().join("SKILL.md"));
            }
        } else if file_type.is_dir() {
            out.extend(Box::pin(collect_markdown(&entry.path(), false)).await?);
        } else if file_type.is_file()
            && entry
                .file_name()
                .to_string_lossy()
                .to_lowercase()
                .ends_with(".md")
        {
            out.push(entry.path());
        }
    }
    Ok(out)
}

/// Maps to: CC `validatePlugin.ts:763-809#validatePluginContents`.
pub async fn validate_plugin_contents(plugin_dir: &str) -> anyhow::Result<Vec<ValidationResult>> {
    let mut results = Vec::new();
    for (file_type, dir) in [
        (ValidationFileType::Skill, "skills"),
        (ValidationFileType::Agent, "agents"),
        (ValidationFileType::Command, "commands"),
    ] {
        let files = collect_markdown(
            &Path::new(plugin_dir).join(dir),
            file_type == ValidationFileType::Skill,
        )
        .await?;
        for file_path in files {
            let content = match fs::read(&file_path).await {
                Ok(content) => String::from_utf8_lossy(&content).into_owned(),
                Err(error) if io_errno_code(&error) == Some("ENOENT") => continue,
                Err(error) => {
                    results.push(ValidationResult {
                        success: false,
                        errors: vec![ValidationError {
                            path: "file".into(),
                            message: format!(
                                "Failed to read: {}",
                                format_native_file_error(&error, "open", Some(&file_path))
                            ),
                            code: None,
                        }],
                        warnings: vec![],
                        file_path: file_path.to_string_lossy().into_owned(),
                        file_type,
                    });
                    continue;
                }
            };
            let result = validate_component_file(&file_path.to_string_lossy(), &content, file_type);
            if !result.errors.is_empty() || !result.warnings.is_empty() {
                results.push(result);
            }
        }
    }
    let hooks_result = validate_hooks_json(
        &Path::new(plugin_dir)
            .join("hooks/hooks.json")
            .to_string_lossy(),
    )
    .await;
    if !hooks_result.errors.is_empty() || !hooks_result.warnings.is_empty() {
        results.push(hooks_result);
    }
    Ok(results)
}

/// Maps to: CC `validatePlugin.ts:814-903#validateManifest`.
pub async fn validate_manifest(file_path: &str) -> anyhow::Result<ValidationResult> {
    let absolute_path = resolve_path(file_path)?;
    let stats = match fs::metadata(&absolute_path).await {
        Ok(stats) => Some(stats),
        Err(error) if io_errno_code(&error) == Some("ENOENT") => None,
        Err(error) => {
            return Err(anyhow::anyhow!(format_native_file_error(
                &error,
                "stat",
                Some(&absolute_path)
            )));
        }
    };
    if stats.is_some_and(|stats| stats.is_dir()) {
        let marketplace_result = validate_marketplace_manifest(
            &absolute_path
                .join(".claude-plugin/marketplace.json")
                .to_string_lossy(),
        )
        .await?;
        if marketplace_result
            .errors
            .first()
            .and_then(|e| e.code.as_deref())
            != Some("ENOENT")
        {
            return Ok(marketplace_result);
        }
        let plugin_result = validate_plugin_manifest(
            &absolute_path
                .join(".claude-plugin/plugin.json")
                .to_string_lossy(),
        )
        .await?;
        if plugin_result.errors.first().and_then(|e| e.code.as_deref()) != Some("ENOENT") {
            return Ok(plugin_result);
        }
        return Ok(ValidationResult{success:false,errors:vec![ValidationError{path:"directory".into(),message:"No manifest found in directory. Expected .claude-plugin/marketplace.json or .claude-plugin/plugin.json".into(),code:None}],warnings:vec![],file_path:absolute_path.to_string_lossy().into_owned(),file_type:ValidationFileType::Plugin});
    }
    match detect_manifest_type(file_path) {
        Some(ValidationFileType::Plugin) => validate_plugin_manifest(file_path).await,
        Some(ValidationFileType::Marketplace) => validate_marketplace_manifest(file_path).await,
        _ => {
            match fs::read(&absolute_path).await {
                Ok(content) => {
                    if let Ok(parsed) = json_parse(&String::from_utf8_lossy(&content))
                        && parsed
                            .get_property("plugins")
                            .and_then(|v| v.as_array())
                            .is_some()
                    {
                        return validate_marketplace_manifest(file_path).await;
                    }
                }
                Err(error) if io_errno_code(&error) == Some("ENOENT") => {
                    return Ok(ValidationResult {
                        success: false,
                        errors: vec![ValidationError {
                            path: "file".into(),
                            message: format!("File not found: {}", absolute_path.display()),
                            code: None,
                        }],
                        warnings: vec![],
                        file_path: absolute_path.to_string_lossy().into_owned(),
                        file_type: ValidationFileType::Plugin,
                    });
                }
                Err(_) => {}
            }
            validate_plugin_manifest(file_path).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct FixtureDirectory(PathBuf);
    impl FixtureDirectory {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("plugin-validate-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for FixtureDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    async fn write_fixture(root: &Path, path: &str, input: &Value) -> String {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).await.unwrap();
        fs::write(&path, serde_json::to_vec(input).unwrap())
            .await
            .unwrap();
        path.to_string_lossy().into_owned()
    }

    #[tokio::test]
    async fn manifest_strict_errors_and_warning_order_match_official_bun() {
        let dir = FixtureDirectory::new();
        let input = json!({"name":"CamelCase","source":"./x","category":"x","tags":["x"],"strict":true,"id":"old"});
        let path = write_fixture(dir.path(), "plugin.json", &input).await;
        let result = validate_plugin_manifest(&path).await.unwrap();
        assert!(result.success);
        assert_eq!(
            result
                .warnings
                .iter()
                .map(|w| w.path.as_str())
                .collect::<Vec<_>>(),
            vec![
                "source",
                "category",
                "tags",
                "strict",
                "id",
                "name",
                "version",
                "description",
                "author"
            ]
        );
        assert_eq!(
            result.warnings[0].message,
            "Field 'source' belongs in the marketplace entry (marketplace.json), not plugin.json. It's harmless here but unused — nyan ignores it at load time."
        );
        assert_eq!(
            result.warnings[5].message,
            "Plugin name \"CamelCase\" is not kebab-case. nyan accepts it, but the Claude.ai marketplace sync requires kebab-case (lowercase letters, digits, and hyphens only, e.g., \"my-plugin\")."
        );
        write_fixture(
            dir.path(),
            "plugin.json",
            &json!({"name":17,"commands":["./a..b","../c"],"agents":"../agent","skills":"../s"}),
        )
        .await;
        let result = validate_plugin_manifest(&path).await.unwrap();
        assert!(!result.success);
        assert!(result.warnings.is_empty());
        assert_eq!(
            result
                .errors
                .iter()
                .map(|e| (e.path.as_str(), e.code.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("commands[0]", None),
                ("commands[1]", None),
                ("agents[0]", None),
                ("skills[0]", None),
                ("name", Some("invalid_type")),
                ("commands", Some("invalid_union")),
                ("agents", Some("invalid_union")),
                ("skills", Some("invalid_union"))
            ]
        );
        assert_eq!(
            result.errors[0].message,
            "Path contains \"..\" which could be a path traversal attempt: ./a..b"
        );
        assert_eq!(
            result.errors[4].message,
            "Invalid input: expected string, received number"
        );
    }

    #[tokio::test]
    async fn marketplace_strict_entries_duplicates_and_version_root_match_official_bun() {
        let dir = FixtureDirectory::new();
        let path=write_fixture(dir.path(),".claude-plugin/marketplace.json",&json!({"name":"my-market","owner":{"name":"A"},"plugins":[{"name":"p","source":"./p","typo":1}],"typo":2})).await;
        let result = validate_marketplace_manifest(&path).await.unwrap();
        assert_eq!(
            result
                .errors
                .iter()
                .map(|e| (e.path.as_str(), e.message.as_str(), e.code.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                (
                    "plugins.0",
                    "Unrecognized key: \"typo\"",
                    Some("unrecognized_keys")
                ),
                (
                    "root",
                    "Unrecognized key: \"typo\"",
                    Some("unrecognized_keys")
                )
            ]
        );
        assert!(result.warnings.is_empty());
        write_fixture(dir.path(),".claude-plugin/marketplace.json",&json!({"name":"my-market","owner":{"name":"A"},"plugins":[{"name":"p","source":"./p","version":"1"},{"name":"p","source":"./p"}]})).await;
        write_fixture(
            dir.path(),
            "p/.claude-plugin/plugin.json",
            &json!({"name":"p","version":"2"}),
        )
        .await;
        let result = validate_manifest(&dir.path().to_string_lossy())
            .await
            .unwrap();
        assert!(!result.success);
        assert_eq!(result.file_type, ValidationFileType::Marketplace);
        assert_eq!(
            result
                .errors
                .iter()
                .map(|e| e.path.as_str())
                .collect::<Vec<_>>(),
            vec!["plugins[0].name", "plugins[1].name"]
        );
        assert!(
            result
                .errors
                .iter()
                .all(|e| e.message == "Duplicate plugin name \"p\" found in marketplace")
        );
        assert_eq!(result.warnings[0],ValidationWarning{path:"plugins[0].version".into(),message:"Entry declares version \"1\" but ./p/.claude-plugin/plugin.json says \"2\". At install time, plugin.json wins (calculatePluginVersion precedence) — the entry version is silently ignored. Update this entry to \"2\" to match.".into()});
        assert_eq!(result.warnings[1].path, "metadata.description");
    }

    #[tokio::test]
    async fn manifest_detection_and_enoent_fallback_match_official_bun() {
        let dir = FixtureDirectory::new();
        let path = dir.path().to_string_lossy().into_owned();
        let result = validate_manifest(&path).await.unwrap();
        assert_eq!(result.errors[0].path, "directory");
        write_fixture(
            dir.path(),
            ".claude-plugin/plugin.json",
            &json!({"name":"ok"}),
        )
        .await;
        assert!(validate_manifest(&path).await.unwrap().success);
        let market = dir.path().join(".claude-plugin/marketplace.json");
        fs::write(&market, "bad").await.unwrap();
        let result = validate_manifest(&path).await.unwrap();
        assert!(!result.success);
        assert_eq!(result.file_type, ValidationFileType::Marketplace);
        assert_eq!(result.errors[0].path, "json");
        assert_eq!(result.errors[0].code, None);
        // Native parser diagnostic is intentionally not claimed byte-equivalent
        // to Bun's `JSON Parse error: Unexpected identifier "bad"`.
        assert_eq!(
            result.errors[0].message,
            format!(
                "Invalid JSON syntax: {}",
                json_parse("bad").err().expect("invalid JSON")
            )
        );
        let named = validate_manifest(&format!("{path}/missing/plugin.json"))
            .await
            .unwrap();
        let unknown = validate_manifest(&format!("{path}/missing/other.json"))
            .await
            .unwrap();
        assert_eq!(named.errors[0].code.as_deref(), Some("ENOENT"));
        assert_eq!(unknown.errors[0].code, None);
        let input = json!({"name":"my-market","owner":{"name":"A"},"plugins":[]});
        let inferred = write_fixture(dir.path(), "unknown.json", &input).await;
        assert_eq!(
            validate_manifest(&inferred).await.unwrap().file_type,
            ValidationFileType::Marketplace
        );
        let hidden = write_fixture(dir.path(), "other/.claude-plugin/custom.json", &input).await;
        let result = validate_manifest(&hidden).await.unwrap();
        assert_eq!(result.file_type, ValidationFileType::Plugin);
        assert_eq!(
            result.errors[0].message,
            "Unrecognized keys: \"owner\", \"plugins\""
        );
        assert_eq!(
            resolve_path(&format!("{path}/one/../e\u{301}")).unwrap(),
            dir.path().join("e\u{301}")
        );
    }

    #[tokio::test]
    async fn manifest_stat_error_matches_official_bun() {
        let dir = FixtureDirectory::new();
        let parent = dir.path().join("file");
        fs::write(&parent, "ordinary file").await.unwrap();
        let child = parent.join("child");
        let error = validate_manifest(child.to_str().unwrap())
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("ENOTDIR: not a directory, stat '{}'", child.display())
        );
    }

    #[test]
    fn component_values_and_exact_frontmatter_delimiters_match_official_bun() {
        let validate =
            |text: &str| validate_component_file("command.md", text, ValidationFileType::Command);
        let absent = validate(" ---\ndescription: ok\n---\n");
        assert!(absent.success);
        assert_eq!(absent.warnings[0].path, "frontmatter");
        let inline_close = validate("---\ndescription: ok---\nbody");
        assert!(inline_close.success);
        assert!(inline_close.warnings.is_empty());
        for (body, kind) in [
            ("", "null"),
            ("null", "null"),
            ("- one", "an array"),
            ("true", "boolean"),
        ] {
            let result = validate(&format!("---\n{body}\n---\n"));
            assert!(!result.success);
            assert_eq!(
                result.errors[0].message,
                format!("Frontmatter must be a YAML mapping (key: value pairs), got {kind}.")
            );
        }
        let result = validate(
            "---\ndescription: [one]\nname: [two]\nallowed-tools: [Read, 1]\nshell: zsh\n---\n",
        );
        assert_eq!(
            result
                .errors
                .iter()
                .map(|e| (e.path.as_str(), e.message.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (
                    "description",
                    "description must be a string, got array. At runtime this value is dropped."
                ),
                ("name", "name must be a string, got object."),
                (
                    "allowed-tools",
                    "allowed-tools array must contain only strings."
                ),
                ("shell", "shell must be 'bash' or 'powershell', got 'zsh'.")
            ]
        );
        for content in [
            "description: null\nname: null\nallowed-tools: null\nshell: null",
            "description: 2\nshell: PowerShell",
        ] {
            let result = validate(&format!("---\n{content}\n---\n"));
            assert!(result.success);
            assert!(result.warnings.is_empty());
        }
    }

    // Native parser diagnostics intentionally remain a documented Bun difference.
    // This checks propagation/wrapping, not byte parity with Bun.YAML errors.
    #[test]
    fn component_yaml_native_error_propagation_retains_context() {
        let validate =
            |text: &str| validate_component_file("command.md", text, ValidationFileType::Command);
        let body = "description: hello: world\n";
        let result = validate(&format!("---\n{body}---\n"));
        assert!(!result.success);
        assert_eq!(
            result.errors[0].message,
            format!(
                "YAML frontmatter failed to parse: {}. At runtime this command loads with empty metadata (all frontmatter fields silently dropped).",
                parse_yaml(body).unwrap_err()
            )
        );
    }

    #[tokio::test]
    async fn content_scan_depth_case_symlinks_and_error_retention_match_official() {
        let dir = FixtureDirectory::new();
        let root = dir.path();
        for (path, text) in [
            ("skills/direct.md", "ignored"),
            ("skills/one/SKILL.md", "body"),
            ("skills/two/nested/SKILL.md", "ignored"),
            ("agents/nested/test.MD", "body"),
            ("commands/clean.md", "---\ndescription: good\n---\n"),
            ("commands/bad.md", "---\nname: 2\n---\n"),
            ("hooks/hooks.json", "{\"hooks\":{\"BadEvent\":[]}}"),
        ] {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).await.unwrap();
            fs::write(path, text).await.unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("commands/bad.md"), root.join("commands/link.md"))
            .unwrap();
        let results = validate_plugin_contents(&root.to_string_lossy())
            .await
            .unwrap();
        assert_eq!(
            results.iter().map(|r| r.file_type).collect::<Vec<_>>(),
            vec![
                ValidationFileType::Skill,
                ValidationFileType::Agent,
                ValidationFileType::Command,
                ValidationFileType::Hooks
            ]
        );
        assert!(results[0].file_path.ends_with("skills/one/SKILL.md"));
        assert!(results[1].file_path.ends_with("agents/nested/test.MD"));
        assert!(results[2].file_path.ends_with("commands/bad.md"));
        assert_eq!(results[3].errors[0].path, "hooks.BadEvent");
        assert_eq!(results[3].errors[0].message, "Invalid key in record");
        assert_eq!(results[3].errors[0].code.as_deref(), Some("invalid_key"));
        fs::remove_file(root.join("hooks/hooks.json"))
            .await
            .unwrap();
        assert!(
            validate_hooks_json(&root.join("hooks/hooks.json").to_string_lossy())
                .await
                .success
        );
    }

    #[test]
    fn marketplace_traversal_hints_match_official_examples() {
        assert_eq!(
            marketplace_source_hint("../../plugins/p"),
            "Plugin source paths are resolved relative to the marketplace root (the directory containing .claude-plugin/), not relative to marketplace.json. Use \"./plugins/p\" instead of \"../../plugins/p\"."
        );
        assert!(marketplace_source_hint("./p/../p").contains("Use \"./plugins/my-plugin\""));
        let mut errors = vec![];
        check_path_traversal("./a..b", "path", &mut errors, None);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, None);
    }
}
