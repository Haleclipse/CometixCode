//! LSP result formatters.
//!
//! Maps to:
//! - CC `tools/LSPTool/formatters.ts`
//! - CC `tools/LSPTool/LSPTool.ts` `formatResult(...)` count extraction.
//!
//! The functions in this module are intentionally data-shaping only. LSP
//! transport, server lifecycle, and Tool.call routing remain in the official
//! sibling boundaries (`services/lsp/*` and `tools/lsp_tool/mod.rs`).

use crate::tools::lsp_tool::schemas::LspOperation;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormattedLspResult {
    pub formatted: String,
    pub result_count: usize,
    pub file_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LocationView<'a> {
    uri: Option<&'a str>,
    range: Option<&'a Value>,
}

/// Maps to: CC `formatUri(...)`.
fn format_uri(uri: Option<&str>, cwd: Option<&str>) -> String {
    let Some(uri) = uri.filter(|uri| !uri.is_empty()) else {
        return "<unknown location>".to_string();
    };

    let mut file_path = uri.strip_prefix("file://").unwrap_or(uri).to_string();
    if is_windows_drive_path_with_leading_slash(&file_path) {
        file_path.remove(0);
    }
    if let Some(decoded) = percent_decode(&file_path) {
        file_path = decoded;
    }

    if let Some(cwd) = cwd {
        if let Some(relative) = relative_path_if_shorter(&file_path, cwd) {
            return relative.replace('\\', "/");
        }
    }

    file_path.replace('\\', "/")
}

fn is_windows_drive_path_with_leading_slash(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3 && bytes[0] == b'/' && bytes[2] == b':' && bytes[1].is_ascii_alphabetic()
}

fn relative_path_if_shorter(file_path: &str, cwd: &str) -> Option<String> {
    let relative = node_like_relative_path(Path::new(cwd), Path::new(file_path))?;
    (!relative.is_empty() && relative.len() < file_path.len() && !relative.starts_with("../../"))
        .then_some(relative)
}

fn node_like_relative_path(from: &Path, to: &Path) -> Option<String> {
    // Maps to Node `path.relative(cwd, filePath)` used by CC. Unlike
    // `strip_prefix`, this can produce `../sibling` when the target is outside
    // cwd but still nearby; CC accepts that unless it starts with `../../`.
    if from.is_absolute() != to.is_absolute() {
        return None;
    }
    let from_parts = path_parts_for_relative(from);
    let to_parts = path_parts_for_relative(to);
    let common = from_parts
        .iter()
        .zip(to_parts.iter())
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative = Vec::new();
    relative.extend(std::iter::repeat("..".to_string()).take(from_parts.len() - common));
    relative.extend(to_parts[common..].iter().cloned());
    Some(relative.join("/"))
}

fn path_parts_for_relative(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            std::path::Component::Prefix(prefix) => {
                Some(prefix.as_os_str().to_string_lossy().to_string())
            }
            std::path::Component::Normal(part) => Some(part.to_string_lossy().to_string()),
            std::path::Component::ParentDir => Some("..".to_string()),
            std::path::Component::CurDir | std::path::Component::RootDir => None,
        })
        .collect()
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return None;
            }
            let hi = hex_value(bytes[index + 1])?;
            let lo = hex_value(bytes[index + 2])?;
            out.push((hi << 4) | lo);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Maps to: CC `groupByFile(...)`.
fn group_locations_by_file<'a>(
    locations: impl IntoIterator<Item = LocationView<'a>>,
    cwd: Option<&str>,
) -> Vec<(String, Vec<LocationView<'a>>)> {
    let mut groups: Vec<(String, Vec<LocationView<'a>>)> = Vec::new();
    for location in locations {
        let file_path = format_uri(location.uri, cwd);
        if let Some((_, items)) = groups.iter_mut().find(|(path, _)| path == &file_path) {
            items.push(location);
        } else {
            groups.push((file_path, vec![location]));
        }
    }
    groups
}

fn group_values_by_file<'a, F>(
    values: impl IntoIterator<Item = &'a Value>,
    cwd: Option<&str>,
    uri: F,
) -> Vec<(String, Vec<&'a Value>)>
where
    F: Fn(&'a Value) -> Option<&'a str>,
{
    let mut groups: Vec<(String, Vec<&'a Value>)> = Vec::new();
    for value in values {
        let file_path = format_uri(uri(value), cwd);
        if let Some((_, items)) = groups.iter_mut().find(|(path, _)| path == &file_path) {
            items.push(value);
        } else {
            groups.push((file_path, vec![value]));
        }
    }
    groups
}

/// Maps to: CC `formatLocation(...)`.
fn format_location(location: &LocationView<'_>, cwd: Option<&str>) -> String {
    let file_path = format_uri(location.uri, cwd);
    let (line, character) = range_start_line_character(location.range);
    format!("{file_path}:{}:{}", line + 1, character + 1)
}

/// Maps to: CC `locationLinkToLocation(...)` / `toLocation(...)`.
fn location_view(value: &Value) -> LocationView<'_> {
    if value.get("targetUri").is_some() {
        LocationView {
            uri: value.get("targetUri").and_then(Value::as_str),
            range: value
                .get("targetSelectionRange")
                .or_else(|| value.get("targetRange")),
        }
    } else {
        LocationView {
            uri: value.get("uri").and_then(Value::as_str),
            range: value.get("range"),
        }
    }
}

fn value_array(value: &Value) -> Vec<&Value> {
    match value {
        Value::Array(values) => values.iter().collect(),
        Value::Null => Vec::new(),
        other => vec![other],
    }
}

fn range_start_line_character(range: Option<&Value>) -> (u64, u64) {
    let line = range
        .and_then(|range| range.pointer("/start/line"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let character = range
        .and_then(|range| range.pointer("/start/character"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    (line, character)
}

/// Maps to: CC `formatGoToDefinitionResult(...)`.
pub fn format_go_to_definition_result(result: &Value, cwd: Option<&str>) -> String {
    if result.is_null() {
        return "No definition found. This may occur if the cursor is not on a symbol, or if the definition is in an external library not indexed by the LSP server.".to_string();
    }

    if !result.is_array() {
        // CC filters malformed locations in array results, but a single
        // Location/LocationLink is formatted directly and `formatUri` supplies
        // `<unknown location>` if the URI is missing.
        let location = location_view(result);
        return format!("Defined in {}", format_location(&location, cwd));
    }

    let raw_locations = value_array(result);
    let valid_locations: Vec<_> = raw_locations
        .iter()
        .map(|value| location_view(value))
        .filter(|location| location.uri.is_some())
        .collect();

    if valid_locations.is_empty() {
        return "No definition found. This may occur if the cursor is not on a symbol, or if the definition is in an external library not indexed by the LSP server.".to_string();
    }

    if valid_locations.len() == 1 {
        return format!("Defined in {}", format_location(&valid_locations[0], cwd));
    }

    let location_list = valid_locations
        .iter()
        .map(|location| format!("  {}", format_location(location, cwd)))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Found {} definitions:\n{location_list}",
        valid_locations.len()
    )
}

/// Maps to: CC `formatFindReferencesResult(...)`.
pub fn format_find_references_result(result: &Value, cwd: Option<&str>) -> String {
    let locations: Vec<_> = match result {
        Value::Array(values) => values.iter().map(location_view).collect(),
        _ => Vec::new(),
    };
    if locations.is_empty() {
        return "No references found. This may occur if the symbol has no usages, or if the LSP server has not fully indexed the workspace.".to_string();
    }

    let valid_locations: Vec<_> = locations
        .into_iter()
        .filter(|location| location.uri.is_some())
        .collect();
    if valid_locations.is_empty() {
        return "No references found. This may occur if the symbol has no usages, or if the LSP server has not fully indexed the workspace.".to_string();
    }

    if valid_locations.len() == 1 {
        return format!(
            "Found 1 reference:\n  {}",
            format_location(&valid_locations[0], cwd)
        );
    }

    let groups = group_locations_by_file(valid_locations.clone(), cwd);
    let mut lines = vec![format!(
        "Found {} references across {} files:",
        valid_locations.len(),
        groups.len()
    )];
    for (file_path, locations) in groups {
        lines.push(format!("\n{file_path}:"));
        for location in locations {
            let (line, character) = range_start_line_character(location.range);
            lines.push(format!("  Line {}:{}", line + 1, character + 1));
        }
    }
    lines.join("\n")
}

/// Maps to: CC `extractMarkupText(...)`.
fn extract_markup_text(contents: &Value) -> String {
    match contents {
        Value::Array(items) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_string)
                    .or_else(|| {
                        item.get("value")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        Value::String(value) => value.clone(),
        Value::Object(map) => map
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

/// Maps to: CC `formatHoverResult(...)`.
pub fn format_hover_result(result: &Value, _cwd: Option<&str>) -> String {
    if result.is_null() {
        return "No hover information available. This may occur if the cursor is not on a symbol, or if the LSP server has not fully indexed the file.".to_string();
    }

    let content = result
        .get("contents")
        .map(extract_markup_text)
        .unwrap_or_default();
    if let Some(range) = result.get("range") {
        let (line, character) = range_start_line_character(Some(range));
        return format!("Hover info at {}:{}:\n\n{content}", line + 1, character + 1);
    }
    content
}

/// Maps to: CC `symbolKindToString(...)`.
fn symbol_kind_to_string(kind: u64) -> &'static str {
    match kind {
        1 => "File",
        2 => "Module",
        3 => "Namespace",
        4 => "Package",
        5 => "Class",
        6 => "Method",
        7 => "Property",
        8 => "Field",
        9 => "Constructor",
        10 => "Enum",
        11 => "Interface",
        12 => "Function",
        13 => "Variable",
        14 => "Constant",
        15 => "String",
        16 => "Number",
        17 => "Boolean",
        18 => "Array",
        19 => "Object",
        20 => "Key",
        21 => "Null",
        22 => "EnumMember",
        23 => "Struct",
        24 => "Event",
        25 => "Operator",
        26 => "TypeParameter",
        _ => "Unknown",
    }
}

/// Maps to: CC `formatDocumentSymbolNode(...)`.
fn format_document_symbol_node(symbol: &Value, indent: usize, lines: &mut Vec<String>) {
    let prefix = "  ".repeat(indent);
    let name = symbol
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let kind = symbol_kind_to_string(symbol.get("kind").and_then(Value::as_u64).unwrap_or(0));
    let mut line = format!("{prefix}{name} ({kind})");
    if let Some(detail) = symbol.get("detail").and_then(Value::as_str) {
        if !detail.is_empty() {
            line.push(' ');
            line.push_str(detail);
        }
    }
    let symbol_line = symbol
        .get("range")
        .and_then(|range| range.pointer("/start/line"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        + 1;
    line.push_str(&format!(" - Line {symbol_line}"));
    lines.push(line);

    if let Some(children) = symbol.get("children").and_then(Value::as_array) {
        for child in children {
            format_document_symbol_node(child, indent + 1, lines);
        }
    }
}

/// Maps to: CC `formatDocumentSymbolResult(...)`.
pub fn format_document_symbol_result(result: &Value, cwd: Option<&str>) -> String {
    let symbols = result.as_array().map(Vec::as_slice).unwrap_or(&[]);
    if symbols.is_empty() {
        return "No symbols found in document. This may occur if the file is empty, not supported by the LSP server, or if the server has not fully indexed the file.".to_string();
    }

    if symbols
        .first()
        .is_some_and(|symbol| symbol.get("location").is_some())
    {
        return format_workspace_symbol_result(result, cwd);
    }

    let mut lines = vec!["Document symbols:".to_string()];
    for symbol in symbols {
        format_document_symbol_node(symbol, 0, &mut lines);
    }
    lines.join("\n")
}

/// Maps to: CC `formatWorkspaceSymbolResult(...)`.
pub fn format_workspace_symbol_result(result: &Value, cwd: Option<&str>) -> String {
    let symbols = result.as_array().map(Vec::as_slice).unwrap_or(&[]);
    if symbols.is_empty() {
        return "No symbols found in workspace. This may occur if the workspace is empty, or if the LSP server has not finished indexing the project.".to_string();
    }

    let valid_symbols: Vec<_> = symbols
        .iter()
        .filter(|symbol| {
            symbol
                .pointer("/location/uri")
                .and_then(Value::as_str)
                .is_some()
        })
        .collect();
    if valid_symbols.is_empty() {
        return "No symbols found in workspace. This may occur if the workspace is empty, or if the LSP server has not finished indexing the project.".to_string();
    }

    let mut lines = vec![format!(
        "Found {} {} in workspace:",
        valid_symbols.len(),
        plural(valid_symbols.len(), "symbol")
    )];
    let groups = group_values_by_file(valid_symbols.iter().copied(), cwd, |symbol| {
        symbol.pointer("/location/uri").and_then(Value::as_str)
    });

    for (file_path, symbols) in groups {
        lines.push(format!("\n{file_path}:"));
        for symbol in symbols {
            let name = symbol
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let kind =
                symbol_kind_to_string(symbol.get("kind").and_then(Value::as_u64).unwrap_or(0));
            let line = symbol
                .pointer("/location/range/start/line")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                + 1;
            let mut symbol_line = format!("  {name} ({kind}) - Line {line}");
            if let Some(container_name) = symbol.get("containerName").and_then(Value::as_str) {
                if !container_name.is_empty() {
                    symbol_line.push_str(" in ");
                    symbol_line.push_str(container_name);
                }
            }
            lines.push(symbol_line);
        }
    }

    lines.join("\n")
}

/// Maps to: CC `formatCallHierarchyItem(...)`.
fn format_call_hierarchy_item(item: &Value, cwd: Option<&str>) -> String {
    let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
    let kind = symbol_kind_to_string(item.get("kind").and_then(Value::as_u64).unwrap_or(0));
    let Some(uri) = item.get("uri").and_then(Value::as_str) else {
        return format!("{name} ({kind}) - <unknown location>");
    };

    let file_path = format_uri(Some(uri), cwd);
    let line = item
        .pointer("/range/start/line")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        + 1;
    let mut result = format!("{name} ({kind}) - {file_path}:{line}");
    if let Some(detail) = item.get("detail").and_then(Value::as_str) {
        if !detail.is_empty() {
            result.push_str(" [");
            result.push_str(detail);
            result.push(']');
        }
    }
    result
}

/// Maps to: CC `formatPrepareCallHierarchyResult(...)`.
pub fn format_prepare_call_hierarchy_result(result: &Value, cwd: Option<&str>) -> String {
    let items = result.as_array().map(Vec::as_slice).unwrap_or(&[]);
    if items.is_empty() {
        return "No call hierarchy item found at this position".to_string();
    }
    if items.len() == 1 {
        return format!(
            "Call hierarchy item: {}",
            format_call_hierarchy_item(&items[0], cwd)
        );
    }

    let mut lines = vec![format!("Found {} call hierarchy items:", items.len())];
    for item in items {
        lines.push(format!("  {}", format_call_hierarchy_item(item, cwd)));
    }
    lines.join("\n")
}

/// Maps to: CC `formatIncomingCallsResult(...)`.
pub fn format_incoming_calls_result(result: &Value, cwd: Option<&str>) -> String {
    let calls = result.as_array().map(Vec::as_slice).unwrap_or(&[]);
    if calls.is_empty() {
        return "No incoming calls found (nothing calls this function)".to_string();
    }

    let mut lines = vec![format!(
        "Found {} incoming {}:",
        calls.len(),
        plural(calls.len(), "call")
    )];
    let calls_with_from = calls
        .iter()
        .filter(|call| call.get("from").is_some())
        .collect::<Vec<_>>();
    let groups = group_values_by_file(calls_with_from, cwd, |call| {
        call.pointer("/from/uri").and_then(Value::as_str)
    });
    for (file_path, calls) in groups {
        lines.push(format!("\n{file_path}:"));
        for call in calls {
            let Some(from) = call.get("from") else {
                continue;
            };
            let name = from.get("name").and_then(Value::as_str).unwrap_or_default();
            let kind = symbol_kind_to_string(from.get("kind").and_then(Value::as_u64).unwrap_or(0));
            let line = from
                .pointer("/range/start/line")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                + 1;
            let mut call_line = format!("  {name} ({kind}) - Line {line}");
            if let Some(ranges) = call.get("fromRanges").and_then(Value::as_array) {
                if !ranges.is_empty() {
                    let call_sites = ranges
                        .iter()
                        .map(|range| {
                            let (line, character) = range_start_line_character(Some(range));
                            format!("{}:{}", line + 1, character + 1)
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    call_line.push_str(&format!(" [calls at: {call_sites}]"));
                }
            }
            lines.push(call_line);
        }
    }
    lines.join("\n")
}

/// Maps to: CC `formatOutgoingCallsResult(...)`.
pub fn format_outgoing_calls_result(result: &Value, cwd: Option<&str>) -> String {
    let calls = result.as_array().map(Vec::as_slice).unwrap_or(&[]);
    if calls.is_empty() {
        return "No outgoing calls found (this function calls nothing)".to_string();
    }

    let mut lines = vec![format!(
        "Found {} outgoing {}:",
        calls.len(),
        plural(calls.len(), "call")
    )];
    let calls_with_to = calls
        .iter()
        .filter(|call| call.get("to").is_some())
        .collect::<Vec<_>>();
    let groups = group_values_by_file(calls_with_to, cwd, |call| {
        call.pointer("/to/uri").and_then(Value::as_str)
    });
    for (file_path, calls) in groups {
        lines.push(format!("\n{file_path}:"));
        for call in calls {
            let Some(to) = call.get("to") else {
                continue;
            };
            let name = to.get("name").and_then(Value::as_str).unwrap_or_default();
            let kind = symbol_kind_to_string(to.get("kind").and_then(Value::as_u64).unwrap_or(0));
            let line = to
                .pointer("/range/start/line")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                + 1;
            let mut call_line = format!("  {name} ({kind}) - Line {line}");
            if let Some(ranges) = call.get("fromRanges").and_then(Value::as_array) {
                if !ranges.is_empty() {
                    let call_sites = ranges
                        .iter()
                        .map(|range| {
                            let (line, character) = range_start_line_character(Some(range));
                            format!("{}:{}", line + 1, character + 1)
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    call_line.push_str(&format!(" [called from: {call_sites}]"));
                }
            }
            lines.push(call_line);
        }
    }
    lines.join("\n")
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        word.to_string()
    } else {
        format!("{word}s")
    }
}

/// Maps to: CC `countSymbols(...)`.
fn count_symbols(symbols: &[Value]) -> usize {
    symbols
        .iter()
        .map(|symbol| {
            1 + symbol
                .get("children")
                .and_then(Value::as_array)
                .map(|children| count_symbols(children))
                .unwrap_or(0)
        })
        .sum()
}

/// Maps to: CC `countUniqueFiles(...)` and call-hierarchy variants.
fn count_unique_uris<'a>(uris: impl IntoIterator<Item = Option<&'a str>>) -> usize {
    uris.into_iter()
        .flatten()
        .filter(|uri| !uri.is_empty())
        .collect::<BTreeSet<_>>()
        .len()
}

/// Maps to: CC `formatResult(...)`.
pub fn format_result(
    operation: LspOperation,
    result: &Value,
    cwd: Option<&str>,
) -> FormattedLspResult {
    match operation {
        LspOperation::GoToDefinition | LspOperation::GoToImplementation => {
            let locations: Vec<_> = value_array(result).into_iter().map(location_view).collect();
            let valid_locations: Vec<_> = locations
                .iter()
                .filter(|location| location.uri.is_some())
                .collect();
            FormattedLspResult {
                formatted: format_go_to_definition_result(result, cwd),
                result_count: valid_locations.len(),
                file_count: count_unique_uris(valid_locations.iter().map(|location| location.uri)),
            }
        }
        LspOperation::FindReferences => {
            let locations: Vec<_> = result
                .as_array()
                .map(|values| values.iter().map(location_view).collect())
                .unwrap_or_default();
            let valid_locations: Vec<_> = locations
                .iter()
                .filter(|location| location.uri.is_some())
                .collect();
            FormattedLspResult {
                formatted: format_find_references_result(result, cwd),
                result_count: valid_locations.len(),
                file_count: count_unique_uris(valid_locations.iter().map(|location| location.uri)),
            }
        }
        LspOperation::Hover => FormattedLspResult {
            formatted: format_hover_result(result, cwd),
            result_count: usize::from(!result.is_null()),
            file_count: usize::from(!result.is_null()),
        },
        LspOperation::DocumentSymbol => {
            let symbols = result.as_array().cloned().unwrap_or_default();
            let is_document_symbol = symbols
                .first()
                .is_some_and(|symbol| symbol.get("range").is_some());
            let count = if is_document_symbol {
                count_symbols(&symbols)
            } else {
                symbols.len()
            };
            FormattedLspResult {
                formatted: format_document_symbol_result(result, cwd),
                result_count: count,
                file_count: usize::from(!symbols.is_empty()),
            }
        }
        LspOperation::WorkspaceSymbol => {
            let symbols = result.as_array().map(Vec::as_slice).unwrap_or(&[]);
            let valid_symbols: Vec<_> = symbols
                .iter()
                .filter(|symbol| {
                    symbol
                        .pointer("/location/uri")
                        .and_then(Value::as_str)
                        .is_some()
                })
                .collect();
            FormattedLspResult {
                formatted: format_workspace_symbol_result(result, cwd),
                result_count: valid_symbols.len(),
                file_count: count_unique_uris(
                    valid_symbols
                        .iter()
                        .map(|symbol| symbol.pointer("/location/uri").and_then(Value::as_str)),
                ),
            }
        }
        LspOperation::PrepareCallHierarchy => {
            let items = result.as_array().map(Vec::as_slice).unwrap_or(&[]);
            FormattedLspResult {
                formatted: format_prepare_call_hierarchy_result(result, cwd),
                result_count: items.len(),
                file_count: count_unique_uris(
                    items
                        .iter()
                        .map(|item| item.get("uri").and_then(Value::as_str)),
                ),
            }
        }
        LspOperation::IncomingCalls => {
            let calls = result.as_array().map(Vec::as_slice).unwrap_or(&[]);
            FormattedLspResult {
                formatted: format_incoming_calls_result(result, cwd),
                result_count: calls.len(),
                file_count: count_unique_uris(
                    calls
                        .iter()
                        .map(|call| call.pointer("/from/uri").and_then(Value::as_str)),
                ),
            }
        }
        LspOperation::OutgoingCalls => {
            let calls = result.as_array().map(Vec::as_slice).unwrap_or(&[]);
            FormattedLspResult {
                formatted: format_outgoing_calls_result(result, cwd),
                result_count: calls.len(),
                file_count: count_unique_uris(
                    calls
                        .iter()
                        .map(|call| call.pointer("/to/uri").and_then(Value::as_str)),
                ),
            }
        }
    }
}

/// Maps to: CC `filterGitIgnoredLocations(...)` in `LSPTool.ts` plus the
/// operation-specific result filtering immediately before `formatResult(...)`.
pub async fn filter_git_ignored_result(operation: LspOperation, result: Value, cwd: &str) -> Value {
    if !matches!(
        operation,
        LspOperation::FindReferences
            | LspOperation::GoToDefinition
            | LspOperation::GoToImplementation
            | LspOperation::WorkspaceSymbol
    ) {
        return result;
    }

    let Value::Array(items) = result else {
        return result;
    };

    if operation == LspOperation::WorkspaceSymbol {
        let locations = items
            .iter()
            .filter_map(|symbol| symbol.get("location"))
            .collect::<Vec<_>>();
        let kept_uris = filter_git_ignored_location_uris(locations, cwd).await;
        return Value::Array(
            items
                .into_iter()
                .filter(|symbol| {
                    let Some(uri) = symbol.pointer("/location/uri").and_then(Value::as_str) else {
                        return true;
                    };
                    kept_uris.contains(uri)
                })
                .collect(),
        );
    }

    let kept_uris = filter_git_ignored_location_uris(items.iter().collect::<Vec<_>>(), cwd).await;
    Value::Array(
        items
            .into_iter()
            .filter(|item| {
                let Some(uri) = to_location_uri(item) else {
                    return true;
                };
                kept_uris.contains(uri)
            })
            .collect(),
    )
}

async fn filter_git_ignored_location_uris(locations: Vec<&Value>, cwd: &str) -> BTreeSet<String> {
    let uri_to_path = locations
        .iter()
        .filter_map(|location| {
            to_location_uri(location).map(|uri| (uri.to_string(), uri_to_file_path(uri)))
        })
        .collect::<Vec<_>>();
    if uri_to_path.is_empty() {
        return BTreeSet::new();
    }

    let mut unique_paths = Vec::<String>::new();
    for (_, path) in &uri_to_path {
        if !unique_paths.contains(path) {
            unique_paths.push(path.clone());
        }
    }

    let mut ignored_paths = BTreeSet::<String>::new();
    for batch in unique_paths.chunks(50) {
        let mut command = tokio::process::Command::new("git");
        // CC inherits process.env (execFileNoThrowWithCwd); the carrier is its counterpart.
        crate::utils::subprocess_env::apply_process_env(&mut command);
        command.arg("check-ignore").args(batch).current_dir(cwd);
        if let Ok(Ok(output)) = tokio::time::timeout(Duration::from_secs(5), command.output()).await
        {
            if output.status.code() == Some(0) {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for line in stdout
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                {
                    ignored_paths.insert(line.to_string());
                }
            }
        }
    }

    uri_to_path
        .into_iter()
        .filter_map(|(uri, path)| (!ignored_paths.contains(&path)).then_some(uri))
        .collect()
}

/// Maps to: CC `uriToFilePath(...)` in `LSPTool.ts`.
pub fn uri_to_file_path(uri: &str) -> String {
    let mut file_path = uri.strip_prefix("file://").unwrap_or(uri).to_string();
    if is_windows_drive_path_with_leading_slash(&file_path) {
        file_path.remove(0);
    }
    percent_decode(&file_path).unwrap_or(file_path)
}

/// Maps to: CC `toLocation(...)` for Tool.call filtering/counting.
pub fn to_location_uri(value: &Value) -> Option<&str> {
    if value.get("targetUri").is_some() {
        value.get("targetUri").and_then(Value::as_str)
    } else {
        value.get("uri").and_then(Value::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(line: u64, character: u64) -> Value {
        serde_json::json!({
            "start": { "line": line, "character": character },
            "end": { "line": line, "character": character + 1 }
        })
    }

    #[test]
    fn format_definition_handles_location_links_and_relative_paths() {
        let result = serde_json::json!([
            {
                "targetUri": "file:///tmp/project/src/main.rs",
                "targetRange": range(9, 2),
                "targetSelectionRange": range(8, 1)
            },
            {
                "uri": "file:///tmp/project/src/lib.rs",
                "range": range(10, 3)
            }
        ]);
        let formatted = format_go_to_definition_result(&result, Some("/tmp/project"));
        assert_eq!(
            formatted,
            "Found 2 definitions:\n  src/main.rs:9:2\n  src/lib.rs:11:4"
        );
    }

    #[test]
    fn format_references_groups_by_file_like_official() {
        let result = serde_json::json!([
            { "uri": "file:///tmp/project/src/main.rs", "range": range(0, 1) },
            { "uri": "file:///tmp/project/src/main.rs", "range": range(2, 3) },
            { "uri": "file:///tmp/project/src/lib.rs", "range": range(4, 5) }
        ]);
        let formatted = format_find_references_result(&result, Some("/tmp/project"));
        assert_eq!(
            formatted,
            "Found 3 references across 2 files:\n\nsrc/main.rs:\n  Line 1:2\n  Line 3:4\n\nsrc/lib.rs:\n  Line 5:6"
        );
    }

    #[test]
    fn format_uri_allows_one_level_parent_relative_path_like_node_relative() {
        assert_eq!(
            format_uri(Some("file:///tmp/sibling/lib.rs"), Some("/tmp/project")),
            "../sibling/lib.rs"
        );
        assert_eq!(
            format_uri(Some("file:///var/log/lib.rs"), Some("/tmp/project")),
            "/var/log/lib.rs"
        );
    }

    #[test]
    fn format_single_definition_with_missing_uri_uses_unknown_location_like_official() {
        let result = serde_json::json!({ "range": range(2, 4) });
        assert_eq!(
            format_go_to_definition_result(&result, Some("/tmp/project")),
            "Defined in <unknown location>:3:5"
        );
    }

    #[test]
    fn format_hover_extracts_markup_content_and_range() {
        let result = serde_json::json!({
            "contents": { "kind": "markdown", "value": "**hello**" },
            "range": range(3, 4)
        });
        assert_eq!(
            format_hover_result(&result, None),
            "Hover info at 4:5:\n\n**hello**"
        );
    }

    #[test]
    fn format_document_symbols_counts_nested_children() {
        let result = serde_json::json!([
            {
                "name": "mod a",
                "kind": 2,
                "detail": "detail",
                "range": range(0, 0),
                "children": [
                    { "name": "f", "kind": 12, "range": range(2, 0) }
                ]
            }
        ]);
        let formatted = format_result(LspOperation::DocumentSymbol, &result, None);
        assert_eq!(formatted.result_count, 2);
        assert_eq!(formatted.file_count, 1);
        assert_eq!(
            formatted.formatted,
            "Document symbols:\nmod a (Module) detail - Line 1\n  f (Function) - Line 3"
        );
    }

    #[test]
    fn format_workspace_symbols_groups_and_counts_files() {
        let result = serde_json::json!([
            {
                "name": "Foo",
                "kind": 5,
                "containerName": "crate",
                "location": { "uri": "file:///tmp/project/src/main.rs", "range": range(1, 0) }
            },
            {
                "name": "bar",
                "kind": 12,
                "location": { "uri": "file:///tmp/project/src/lib.rs", "range": range(4, 0) }
            }
        ]);
        let formatted = format_result(LspOperation::WorkspaceSymbol, &result, Some("/tmp/project"));
        assert_eq!(formatted.result_count, 2);
        assert_eq!(formatted.file_count, 2);
        assert_eq!(
            formatted.formatted,
            "Found 2 symbols in workspace:\n\nsrc/main.rs:\n  Foo (Class) - Line 2 in crate\n\nsrc/lib.rs:\n  bar (Function) - Line 5"
        );
    }

    #[test]
    fn format_call_hierarchy_calls_match_official_copy() {
        let incoming = serde_json::json!([
            {
                "from": {
                    "name": "caller",
                    "kind": 12,
                    "uri": "file:///tmp/project/src/main.rs",
                    "range": range(6, 0)
                },
                "fromRanges": [range(7, 2), range(8, 3)]
            }
        ]);
        let formatted = format_result(LspOperation::IncomingCalls, &incoming, Some("/tmp/project"));
        assert_eq!(formatted.result_count, 1);
        assert_eq!(formatted.file_count, 1);
        assert_eq!(
            formatted.formatted,
            "Found 1 incoming call:\n\nsrc/main.rs:\n  caller (Function) - Line 7 [calls at: 8:3, 9:4]"
        );
    }

    #[test]
    fn format_call_hierarchy_skips_entries_missing_from_or_to_without_empty_header() {
        let incoming = serde_json::json!([
            { "fromRanges": [range(0, 0)] },
            {
                "from": {
                    "name": "caller",
                    "kind": 12,
                    "uri": "file:///tmp/project/src/main.rs",
                    "range": range(6, 0)
                }
            }
        ]);
        let formatted = format_incoming_calls_result(&incoming, Some("/tmp/project"));
        assert_eq!(
            formatted,
            "Found 2 incoming calls:\n\nsrc/main.rs:\n  caller (Function) - Line 7"
        );
    }

    #[tokio::test]
    async fn filter_git_ignored_result_removes_ignored_locations_like_official() {
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("cometix-lsp-ignore-{unique}"));
        std::fs::create_dir_all(&root).unwrap();
        let _ = std::process::Command::new("git")
            .arg("init")
            .arg("--quiet")
            .current_dir(&root)
            .status();
        std::fs::write(root.join(".gitignore"), "ignored.rs\n").unwrap();
        std::fs::write(root.join("kept.rs"), "fn kept() {}\n").unwrap();
        std::fs::write(root.join("ignored.rs"), "fn ignored() {}\n").unwrap();

        let result = serde_json::json!([
            { "uri": path_to_uri(&root.join("kept.rs")), "range": range(0, 0) },
            { "uri": path_to_uri(&root.join("ignored.rs")), "range": range(1, 0) }
        ]);
        let filtered =
            filter_git_ignored_result(LspOperation::FindReferences, result, root.to_str().unwrap())
                .await;
        let filtered = filtered.as_array().unwrap();
        assert_eq!(filtered.len(), 1);
        assert!(filtered[0]["uri"].as_str().unwrap().ends_with("kept.rs"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn uri_to_file_path_decodes_and_handles_malformed_uri_fallback() {
        assert_eq!(uri_to_file_path("file:///tmp/a%20b.rs"), "/tmp/a b.rs");
        assert_eq!(uri_to_file_path("file:///tmp/%zz.rs"), "/tmp/%zz.rs");
    }

    fn path_to_uri(path: &std::path::Path) -> String {
        let mut normalized = path.to_string_lossy().replace('\\', "/");
        if !normalized.starts_with('/') {
            normalized = format!("/{normalized}");
        }
        format!("file://{normalized}")
    }
}
