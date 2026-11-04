//! Wire types and conversions for Google Gemini API (v1beta).
//!
//! @nyan: docs/nyan/provider-redesign.md §3.5, §7.4. Converts provider-neutral
//! IR to Gemini GenerateContentRequest and parses incoming stream chunks.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::services::provider::adapter::ResolvedTarget;
use crate::services::provider::ir::{
    AssistantPart, MediaSource, Message, ModelRequest, SYSTEM_BOUNDARY, ThinkingPreference,
    ToolChoice, ToolResultContent, ToolResultPart, ToolSpec,
};

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GenerateContentRequest {
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub contents: Vec<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<Content>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tools: Vec<Tool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_config: Option<ToolConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_config: Option<GenerationConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Content {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub parts: Vec<Part>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Part {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thought: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inline_data: Option<InlineData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_call: Option<FunctionCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_response: Option<FunctionResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InlineData {
    pub mime_type: String,
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FunctionCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub args: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FunctionResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub response: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub function_declarations: Vec<FunctionDeclaration>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FunctionDeclaration {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_calling_config: Option<FunctionCallingConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FunctionCallingConfig {
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_function_names: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_schema: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_config: Option<ThinkingConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<i32>,
}

// ---------------------------------------------------------------------------
// Response Chunk (SSE)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GenerateContentResponse {
    #[serde(default)]
    pub candidates: Vec<Candidate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_metadata: Option<UsageMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UsageMetadata {
    #[serde(default)]
    pub prompt_token_count: u64,
    #[serde(default)]
    pub candidates_token_count: u64,
    #[serde(default)]
    pub total_token_count: u64,
    #[serde(default)]
    pub cached_content_token_count: u64,
    #[serde(default)]
    pub thoughts_token_count: u64,
}

// ---------------------------------------------------------------------------
// Mapping IR -> Wire
// ---------------------------------------------------------------------------

pub fn build_system_instruction(system: &[String]) -> Option<Content> {
    let combined = system
        .iter()
        .map(|s| s.replace(SYSTEM_BOUNDARY, ""))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");

    if combined.is_empty() {
        None
    } else {
        Some(Content {
            role: None,
            parts: vec![Part {
                text: Some(combined),
                ..Default::default()
            }],
        })
    }
}

fn build_tools(tools: &[ToolSpec]) -> Vec<Tool> {
    let function_declarations = tools
        .iter()
        .filter(|t| t.name != "ToolSearch")
        .map(|t| FunctionDeclaration {
            name: t.name.clone(),
            description: t.description.clone(),
            parameters: Some(t.input_schema.clone()),
        })
        .collect::<Vec<_>>();

    if function_declarations.is_empty() {
        Vec::new()
    } else {
        vec![Tool {
            function_declarations,
        }]
    }
}

fn build_tool_config(choice: &ToolChoice) -> Option<ToolConfig> {
    let config = match choice {
        ToolChoice::Auto => FunctionCallingConfig {
            mode: "AUTO".into(),
            allowed_function_names: None,
        },
        ToolChoice::Any => FunctionCallingConfig {
            mode: "ANY".into(),
            allowed_function_names: None,
        },
        ToolChoice::None => FunctionCallingConfig {
            mode: "NONE".into(),
            allowed_function_names: None,
        },
        ToolChoice::Tool { name } => FunctionCallingConfig {
            mode: "ANY".into(),
            allowed_function_names: Some(vec![name.clone()]),
        },
    };
    Some(ToolConfig {
        function_calling_config: Some(config),
    })
}

/// Convert a neutral [`ModelRequest`] to Gemini's [`GenerateContentRequest`].
pub fn build_request(request: &ModelRequest, target: &ResolvedTarget) -> GenerateContentRequest {
    // 1. Build tool call id -> tool name map from Assistant messages
    let mut tool_name_map = BTreeMap::new();
    for message in &request.messages {
        if let Message::Assistant { parts } = message {
            for part in parts {
                if let AssistantPart::ToolCall { id, name, .. } = part {
                    tool_name_map.insert(id.clone(), name.clone());
                }
            }
        }
    }

    // 2. Build contents, coalescing consecutive messages with the same role
    // to strictly adhere to Gemini's alternation requirement.
    let mut contents: Vec<Content> = Vec::new();

    for message in &request.messages {
        let (role_str, new_parts) = match message {
            Message::User { parts } => {
                let mut wire_parts = Vec::new();
                for part in parts {
                    match part {
                        crate::services::provider::ir::UserPart::Text { text } => {
                            wire_parts.push(Part {
                                text: Some(text.clone()),
                                ..Default::default()
                            });
                        }
                        crate::services::provider::ir::UserPart::Image { source, .. } => {
                            if let MediaSource::Base64 { media_type, data } = source {
                                wire_parts.push(Part {
                                    inline_data: Some(InlineData {
                                        mime_type: media_type.clone(),
                                        data: data.clone(),
                                    }),
                                    ..Default::default()
                                });
                            }
                        }
                        crate::services::provider::ir::UserPart::Document { media_type, data } => {
                            wire_parts.push(Part {
                                inline_data: Some(InlineData {
                                    mime_type: media_type.clone(),
                                    data: data.clone(),
                                }),
                                ..Default::default()
                            });
                        }
                        crate::services::provider::ir::UserPart::ToolResult {
                            tool_call_id,
                            content,
                            is_error: _,
                        } => {
                            let fn_name = tool_name_map
                                .get(tool_call_id)
                                .cloned()
                                .unwrap_or_else(|| tool_call_id.clone());
                            let response_val = match content {
                                ToolResultContent::Text(text) => {
                                    serde_json::json!({ "output": text })
                                }
                                ToolResultContent::Parts(parts) => {
                                    let text = parts
                                        .iter()
                                        .filter_map(|p| match p {
                                            ToolResultPart::Text { text } => Some(text.as_str()),
                                            _ => None,
                                        })
                                        .collect::<Vec<_>>()
                                        .join("\n");
                                    serde_json::json!({ "output": text })
                                }
                            };
                            wire_parts.push(Part {
                                function_response: Some(FunctionResponse {
                                    id: Some(tool_call_id.clone()),
                                    name: fn_name,
                                    response: response_val,
                                }),
                                ..Default::default()
                            });
                        }
                    }
                }
                ("user", wire_parts)
            }
            Message::Assistant { parts } => {
                let mut wire_parts = Vec::new();
                for part in parts {
                    match part {
                        AssistantPart::Text { text } => {
                            wire_parts.push(Part {
                                text: Some(text.clone()),
                                ..Default::default()
                            });
                        }
                        AssistantPart::Thinking {
                            text,
                            signature,
                            provider_state: _,
                        } => {
                            let thought_sig = if !signature.is_empty() {
                                Some(signature.clone())
                            } else {
                                None
                            };
                            wire_parts.push(Part {
                                text: Some(text.clone()),
                                thought: Some(true),
                                thought_signature: thought_sig,
                                ..Default::default()
                            });
                        }
                        AssistantPart::RedactedThinking {
                            data,
                            provider_state: _,
                        } => {
                            wire_parts.push(Part {
                                text: Some(data.clone()),
                                thought: Some(true),
                                ..Default::default()
                            });
                        }
                        AssistantPart::ToolCall {
                            id,
                            name,
                            input,
                            provider_state: _,
                        } => {
                            wire_parts.push(Part {
                                function_call: Some(FunctionCall {
                                    id: Some(id.clone()),
                                    name: name.clone(),
                                    args: input.clone(),
                                }),
                                ..Default::default()
                            });
                        }
                        AssistantPart::ServerToolCall { .. }
                        | AssistantPart::ServerToolResult { .. } => {}
                    }
                }
                ("model", wire_parts)
            }
        };

        if new_parts.is_empty() {
            continue;
        }

        if let Some(last) = contents.last_mut()
            && last.role.as_deref() == Some(role_str)
        {
            last.parts.extend(new_parts);
        } else {
            contents.push(Content {
                role: Some(role_str.to_string()),
                parts: new_parts,
            });
        }
    }

    // 3. System instruction
    let system_instruction = build_system_instruction(&request.system);

    // 4. Tools and ToolConfig
    let tools = if target.capabilities.tools {
        build_tools(&request.tools)
    } else {
        Vec::new()
    };

    let tool_config = if !tools.is_empty() {
        build_tool_config(&request.tool_choice)
    } else {
        None
    };

    // 5. GenerationConfig
    let mut gen_config = GenerationConfig::default();
    if let Some(temp) = request.temperature {
        gen_config.temperature = Some(temp);
    }
    let max_tokens = request.max_output_tokens.min(target.max_output_tokens);
    if max_tokens > 0 {
        gen_config.max_output_tokens = Some(max_tokens);
    }
    if target.capabilities.structured_output
        && let Some(structured) = &request.structured_output
    {
        gen_config.response_mime_type = Some("application/json".into());
        gen_config.response_schema = Some(structured.schema.clone());
    }
    match request.thinking {
        ThinkingPreference::Disabled => {}
        ThinkingPreference::Adaptive => {
            gen_config.thinking_config = Some(ThinkingConfig {
                thinking_budget: Some(-1),
            });
        }
        ThinkingPreference::Budget { tokens } => {
            let budget = tokens.map(|t| t as i32).unwrap_or(0);
            gen_config.thinking_config = Some(ThinkingConfig {
                thinking_budget: Some(budget),
            });
        }
    }

    let generation_config = if gen_config != GenerationConfig::default() {
        Some(gen_config)
    } else {
        None
    };

    GenerateContentRequest {
        contents,
        system_instruction,
        tools,
        tool_config,
        generation_config,
    }
}
