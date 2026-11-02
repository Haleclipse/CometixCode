//! Maps to: CC `utils/notebook.ts`.

use serde_json::Value;

/// Maps to: CC `utils/notebook.ts#parseCellId`.
pub fn parse_cell_id(cell_id: &str) -> Option<usize> {
    let digits = cell_id.strip_prefix("cell-")?;
    if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    digits.parse::<usize>().ok()
}

const LARGE_OUTPUT_THRESHOLD: usize = 10_000;

/// Maps to: CC `NotebookCellSource` / processed cell payload from
/// `readNotebook`. Optional fields represent JavaScript `undefined` and are
/// declared in source insertion order because the serialized bytes feed size,
/// token, and Read-state policy.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NotebookCellSource {
    #[serde(rename = "cellType", skip_serializing_if = "Option::is_none")]
    pub cell_type: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_count: Option<Value>,
    pub cell_id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outputs: Option<Vec<Option<NotebookCellSourceOutput>>>,
}

/// Maps to: CC `NotebookCellSourceOutput`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NotebookCellSourceOutput {
    pub output_type: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<Value>,
}

fn javascript_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value
            .as_f64()
            .is_some_and(|value| value != 0.0 && !value.is_nan()),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn javascript_to_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value
            .as_f64()
            .map(|value| ryu_js::Buffer::new().format(value).to_string())
            .unwrap_or_else(|| value.to_string()),
        Value::String(value) => value.clone(),
        Value::Array(values) => values
            .iter()
            .map(|value| match value {
                Value::Null => String::new(),
                value => javascript_to_string(value),
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_string(),
    }
}

fn javascript_optional_to_string(value: Option<&Value>) -> String {
    value
        .map(javascript_to_string)
        .unwrap_or_else(|| "undefined".to_string())
}

fn property<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    value.as_object().and_then(|object| object.get(name))
}

fn joined_array(values: &[Value], separator: &str) -> String {
    values
        .iter()
        .map(|value| match value {
            Value::Null => String::new(),
            value => javascript_to_string(value),
        })
        .collect::<Vec<_>>()
        .join(separator)
}

/// Maps to: CC `utils/notebook.ts#processOutputText`.
fn process_output_text(text: Option<&Value>) -> Result<Value, String> {
    let Some(text) = text.filter(|value| javascript_truthy(value)) else {
        return Ok(Value::String(String::new()));
    };
    let raw = match text {
        Value::String(value) => value.clone(),
        Value::Array(values) => joined_array(values, ""),
        // `formatOutput` reaches `content.slice(...)` for ordinary truthy
        // non-string replacements.
        _ => return Err("content.slice is not a function".to_string()),
    };
    Ok(Value::String(
        crate::tools::bash_tool::utils::format_output(&raw).truncated_content,
    ))
}

/// Maps to: CC `utils/notebook.ts#extractImage`.
fn extract_image(data: &Value) -> Option<Value> {
    let strip_ecmascript_whitespace = |value: &str| {
        value
            .chars()
            .filter(|character| {
                !matches!(
                    *character,
                    '\u{0009}'..='\u{000d}'
                        | '\u{0020}'
                        | '\u{00a0}'
                        | '\u{1680}'
                        | '\u{2000}'..='\u{200a}'
                        | '\u{2028}'
                        | '\u{2029}'
                        | '\u{202f}'
                        | '\u{205f}'
                        | '\u{3000}'
                        | '\u{feff}'
                )
            })
            .collect::<String>()
    };
    let object = data.as_object()?;
    if let Some(png) = object.get("image/png").and_then(Value::as_str) {
        return Some(serde_json::json!({
            "image_data": strip_ecmascript_whitespace(png),
            "media_type": "image/png",
        }));
    }
    if let Some(jpeg) = object.get("image/jpeg").and_then(Value::as_str) {
        return Some(serde_json::json!({
            "image_data": strip_ecmascript_whitespace(jpeg),
            "media_type": "image/jpeg",
        }));
    }
    None
}

/// Maps to: CC `utils/notebook.ts#processOutput`.
fn process_output(output: &Value) -> Result<Option<NotebookCellSourceOutput>, String> {
    if output.is_null() {
        return Err("Cannot read properties of null (reading 'output_type')".to_string());
    }
    let Some(output_type) = property(output, "output_type")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return Ok(None);
    };
    Ok(match output_type.as_str() {
        "stream" => Some(NotebookCellSourceOutput {
            output_type: Value::String(output_type),
            text: Some(process_output_text(property(output, "text"))?),
            image: None,
        }),
        "execute_result" | "display_data" => {
            let data = property(output, "data");
            let text = data.and_then(|data| property(data, "text/plain"));
            let image = match data {
                None => None,
                Some(data) if !javascript_truthy(data) => Some(data.clone()),
                Some(data) => extract_image(data),
            };
            Some(NotebookCellSourceOutput {
                output_type: Value::String(output_type),
                text: Some(process_output_text(text)?),
                image,
            })
        }
        "error" => {
            let traceback = match property(output, "traceback") {
                None => {
                    return Err("Cannot read properties of undefined (reading 'join')".to_string());
                }
                Some(Value::Null) => {
                    return Err("Cannot read properties of null (reading 'join')".to_string());
                }
                Some(Value::Array(values)) => joined_array(values, "\n"),
                Some(_) => return Err("output.traceback.join is not a function".to_string()),
            };
            let raw = format!(
                "{}: {}\n{traceback}",
                javascript_optional_to_string(property(output, "ename")),
                javascript_optional_to_string(property(output, "evalue")),
            );
            Some(NotebookCellSourceOutput {
                output_type: Value::String(output_type),
                text: Some(process_output_text(Some(&Value::String(raw)))?),
                image: None,
            })
        }
        _ => None,
    })
}

fn javascript_length(value: &Value) -> Option<usize> {
    match value {
        Value::String(value) => Some(value.encode_utf16().count()),
        Value::Array(values) => Some(values.len()),
        Value::Object(object) => object
            .get("length")
            .and_then(Value::as_f64)
            .filter(|length| length.is_finite() && *length >= 0.0)
            .map(|length| length as usize),
        Value::Null | Value::Bool(_) | Value::Number(_) => None,
    }
}

/// Maps to: CC `utils/notebook.ts#isLargeOutputs`.
fn is_large_outputs(outputs: &[Option<NotebookCellSourceOutput>]) -> Result<bool, String> {
    let mut size = 0usize;
    for output in outputs.iter().flatten() {
        size = size.saturating_add(
            output
                .text
                .as_ref()
                .and_then(javascript_length)
                .unwrap_or(0),
        );
        if let Some(image) = output.image.as_ref().filter(|image| !image.is_null()) {
            let image_data = property(image, "image_data").ok_or_else(|| {
                "Cannot read properties of undefined (reading 'length')".to_string()
            })?;
            if image_data.is_null() {
                return Err("Cannot read properties of null (reading 'length')".to_string());
            }
            size = size.saturating_add(javascript_length(image_data).unwrap_or(0));
        }
        if size > LARGE_OUTPUT_THRESHOLD {
            return Ok(true);
        }
    }
    Ok(false)
}

fn has_truthy_length(value: &Value) -> bool {
    match value {
        Value::String(value) => !value.is_empty(),
        Value::Array(values) => !values.is_empty(),
        Value::Object(object) => object.get("length").is_some_and(javascript_truthy),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

/// Maps to: CC `utils/notebook.ts#processCell`.
fn process_cell(
    cell: &Value,
    index: usize,
    code_language: &Value,
    include_large_outputs: bool,
) -> Result<NotebookCellSource, String> {
    if cell.is_null() {
        return Err("Cannot read properties of null (reading 'id')".to_string());
    }
    let cell_type = property(cell, "cell_type").cloned();
    let is_code = cell_type.as_ref().and_then(Value::as_str) == Some("code");
    let source = property(cell, "source").map(|source| match source {
        Value::Array(values) => Value::String(joined_array(values, "")),
        source => source.clone(),
    });
    let execution_count = is_code
        .then(|| property(cell, "execution_count"))
        .flatten()
        .filter(|value| javascript_truthy(value))
        .cloned();
    let cell_id = property(cell, "id")
        .filter(|value| !value.is_null())
        .cloned()
        .unwrap_or_else(|| Value::String(format!("cell-{index}")));
    let language = is_code.then(|| code_language.clone());

    let mut cell_data = NotebookCellSource {
        cell_type,
        source,
        execution_count,
        cell_id,
        language,
        outputs: None,
    };

    if is_code
        && let Some(raw_outputs) = property(cell, "outputs").filter(|value| !value.is_null())
        && has_truthy_length(raw_outputs)
    {
        let Value::Array(raw_outputs) = raw_outputs else {
            return Err("cell.outputs.map is not a function".to_string());
        };
        let outputs = raw_outputs
            .iter()
            .map(process_output)
            .collect::<Result<Vec<_>, _>>()?;
        if !include_large_outputs && is_large_outputs(&outputs)? {
            cell_data.outputs = Some(vec![Some(NotebookCellSourceOutput {
                output_type: Value::String("stream".to_string()),
                text: Some(Value::String(format!(
                    "Outputs are too large to include. Use Bash with: cat <notebook_path> | jq '.cells[{index}].outputs'"
                ))),
                image: None,
            })]);
        } else {
            cell_data.outputs = Some(outputs);
        }
    }

    Ok(cell_data)
}

/// Maps to: CC `utils/notebook.ts:164-183#readNotebook`.
///
/// The processed representation deliberately retains JavaScript omission,
/// coercion, and native throw positions before FileRead serializes it.
pub fn read_notebook(
    notebook_path: &std::path::Path,
    cell_id: Option<&str>,
) -> anyhow::Result<Vec<NotebookCellSource>> {
    let bytes = futures::executor::block_on(
        crate::utils::fs_operations::get_fs_implementation().read_file_bytes(notebook_path, None),
    )?;
    let content = String::from_utf8_lossy(&bytes);
    let notebook: Value = serde_json::from_str(&content)?;
    if notebook.is_null() {
        return Err(anyhow::anyhow!(
            "Cannot read properties of null (reading 'metadata')"
        ));
    }
    let metadata = property(&notebook, "metadata").ok_or_else(|| {
        anyhow::anyhow!("Cannot read properties of undefined (reading 'language_info')")
    })?;
    if metadata.is_null() {
        return Err(anyhow::anyhow!(
            "Cannot read properties of null (reading 'language_info')"
        ));
    }
    let language = property(metadata, "language_info")
        .filter(|value| !value.is_null())
        .and_then(|language_info| property(language_info, "name"))
        .filter(|name| !name.is_null())
        .cloned()
        .unwrap_or_else(|| Value::String("python".to_string()));
    let cells_value = property(&notebook, "cells")
        .ok_or_else(|| anyhow::anyhow!("Cannot read properties of undefined (reading 'map')"))?;
    let cells = cells_value.as_array().ok_or_else(|| {
        anyhow::anyhow!(if cell_id.is_some() {
            "notebook.cells.find is not a function"
        } else {
            "notebook.cells.map is not a function"
        })
    })?;

    if let Some(cell_id) = cell_id {
        let (index, cell) = cells
            .iter()
            .enumerate()
            .find(|(_, cell)| property(cell, "id").and_then(Value::as_str) == Some(cell_id))
            .ok_or_else(|| anyhow::anyhow!("Cell with ID \"{cell_id}\" not found in notebook"))?;
        return Ok(vec![
            process_cell(cell, index, &language, true).map_err(anyhow::Error::msg)?,
        ]);
    }

    cells
        .iter()
        .enumerate()
        .map(|(index, cell)| {
            process_cell(cell, index, &language, false).map_err(anyhow::Error::msg)
        })
        .collect()
}

/// Maps to: CC `utils/notebook.ts:188-215#mapNotebookCellsToToolResult`.
pub fn map_notebook_cells_to_tool_result(
    cells: &[NotebookCellSource],
) -> Result<Vec<Value>, String> {
    fn push_text(blocks: &mut Vec<Value>, text: String) {
        if let Some(previous) = blocks
            .last_mut()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            && let Some(previous_text) = previous
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_string)
        {
            previous["text"] = Value::String(format!("{previous_text}\n{text}"));
            return;
        }
        blocks.push(serde_json::json!({"text": text, "type": "text"}));
    }

    let mut blocks = Vec::<Value>::new();
    for cell in cells {
        let mut metadata = String::new();
        let is_code = cell.cell_type.as_ref().and_then(Value::as_str) == Some("code");
        if !is_code {
            metadata.push_str(&format!(
                "<cell_type>{}</cell_type>",
                javascript_optional_to_string(cell.cell_type.as_ref())
            ));
        }
        if is_code
            && cell
                .language
                .as_ref()
                .is_none_or(|language| language.as_str() != Some("python"))
        {
            metadata.push_str(&format!(
                "<language>{}</language>",
                javascript_optional_to_string(cell.language.as_ref())
            ));
        }
        let cell_id = javascript_to_string(&cell.cell_id);
        push_text(
            &mut blocks,
            format!(
                "<cell id=\"{cell_id}\">{metadata}{}</cell id=\"{cell_id}\">",
                javascript_optional_to_string(cell.source.as_ref())
            ),
        );
        if let Some(outputs) = &cell.outputs {
            for output in outputs {
                let output = output.as_ref().ok_or_else(|| {
                    "Cannot read properties of undefined (reading 'text')".to_string()
                })?;
                if let Some(text) = output.text.as_ref().filter(|text| javascript_truthy(text)) {
                    push_text(&mut blocks, format!("\n{}", javascript_to_string(text)));
                }
                if let Some(image) = output
                    .image
                    .as_ref()
                    .filter(|image| javascript_truthy(image))
                {
                    let image_data = property(image, "image_data")
                        .cloned()
                        .unwrap_or(Value::Null);
                    let media_type = property(image, "media_type")
                        .cloned()
                        .unwrap_or(Value::Null);
                    blocks.push(serde_json::json!({
                        "type": "image",
                        "source": {
                            "data": image_data,
                            "media_type": media_type,
                            "type": "base64"
                        }
                    }));
                }
            }
        }
    }
    Ok(blocks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_notebook(name: &str, content: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "nyan-notebook-{name}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("notebook.ipynb");
        std::fs::write(&path, content).unwrap();
        (root, path)
    }

    #[test]
    fn parse_cell_id_matches_official_cell_dash_index_pattern() {
        assert_eq!(parse_cell_id("cell-0"), Some(0));
        assert_eq!(parse_cell_id("cell-42"), Some(42));
        assert_eq!(parse_cell_id("cell-a"), None);
        assert_eq!(parse_cell_id("42"), None);
        assert_eq!(parse_cell_id("cell-"), None);
    }

    #[test]
    fn processed_json_property_order_and_omission_match_official() {
        let (root, path) = temp_notebook(
            "property-order",
            r##"{
              "metadata": {"language_info": {"name": "rust"}},
              "cells": [
                {"cell_type": "markdown", "id": "md0", "source": ["# ", "Title"]},
                {"cell_type": "code", "id": "c0", "execution_count": 2, "source": ["1"], "outputs": [{"output_type":"stream","text":["ok", "\n"]}]}
              ]
            }"##,
        );
        let cells = read_notebook(&path, None).unwrap();
        assert_eq!(
            serde_json::to_string(&cells).unwrap(),
            r##"[{"cellType":"markdown","source":"# Title","cell_id":"md0"},{"cellType":"code","source":"1","execution_count":2,"cell_id":"c0","language":"rust","outputs":[{"output_type":"stream","text":"ok\n"}]}]"##
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn malformed_error_output_fails_while_processing_traceback() {
        let (root, path) = temp_notebook(
            "malformed-error",
            r#"{
              "metadata": {},
              "cells": [{
                "cell_type": "code",
                "source": "1",
                "outputs": [{"output_type": "error", "ename": "Error", "evalue": "bad"}]
              }]
            }"#,
        );
        assert_eq!(
            read_notebook(&path, None).unwrap_err().to_string(),
            "Cannot read properties of undefined (reading 'join')"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unknown_output_serializes_as_null_and_fails_mapper_like_official() {
        let (root, path) = temp_notebook(
            "unknown-output",
            r#"{
              "metadata": {"language_info": {"name": "python"}},
              "cells": [{
                "cell_type": "code",
                "id": "c0",
                "source": "1",
                "outputs": [{"output_type": "future_output", "payload": true}]
              }]
            }"#,
        );

        let cells = read_notebook(&path, None).unwrap();
        assert_eq!(
            serde_json::to_value(&cells).unwrap()[0]["outputs"][0],
            Value::Null
        );
        assert_eq!(
            map_notebook_cells_to_tool_result(&cells),
            Err("Cannot read properties of undefined (reading 'text')".to_string())
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn source_array_uses_javascript_join_coercion() {
        let (root, path) = temp_notebook(
            "source-coercion",
            r#"{
              "metadata": {},
              "cells": [{
                "cell_type": "markdown",
                "source": ["a", null, true, 2, ["x", "y"], {"k":1}]
              }]
            }"#,
        );
        let cells = read_notebook(&path, None).unwrap();
        assert_eq!(
            cells[0].source,
            Some(Value::String("atrue2x,y[object Object]".to_string()))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn read_notebook_extracts_code_and_markdown_cells() {
        let (root, path) = temp_notebook(
            "cells",
            r##"{
              "metadata": {"language_info": {"name": "python"}},
              "cells": [
                {"cell_type": "markdown", "id": "md0", "source": ["# Title"]},
                {"cell_type": "code", "id": "c0", "execution_count": 1, "source": ["print(1)"], "outputs": []}
              ]
            }"##,
        );

        let cells = read_notebook(&path, None).unwrap();
        assert_eq!(cells.len(), 2);
        assert_eq!(
            cells[0].cell_type,
            Some(Value::String("markdown".to_string()))
        );
        assert_eq!(cells[0].source, Some(Value::String("# Title".to_string())));
        assert_eq!(cells[1].cell_type, Some(Value::String("code".to_string())));
        assert_eq!(cells[1].source, Some(Value::String("print(1)".to_string())));
        assert_eq!(cells[1].execution_count, Some(serde_json::json!(1)));
        let _ = std::fs::remove_dir_all(root);
    }
}
