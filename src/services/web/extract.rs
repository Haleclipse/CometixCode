//! Prompt extraction with literal snapshot validation.
//!
//! @nyan: the model is allowed to choose excerpts, but it is never trusted to
//! rewrite them. Every returned item must occur byte-for-byte in the snapshot;
//! rejected items are counted in the result instead of silently disappearing.

use crate::tool::AbortController;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractedExcerpts {
    pub excerpts: Vec<String>,
    pub discarded: usize,
}

impl ExtractedExcerpts {
    pub fn render(&self) -> String {
        let mut result = self.excerpts.join("\n\n");
        if self.discarded > 0 {
            if !result.is_empty() {
                result.push_str("\n\n");
            }
            result.push_str(&format!(
                "[{} excerpts discarded (not found in page)]",
                self.discarded
            ));
        }
        if result.is_empty() {
            "No matching excerpts found in page.".to_string()
        } else {
            result
        }
    }
}

pub fn validate_excerpts<'a, I>(candidates: I, page: &str) -> ExtractedExcerpts
where
    I: IntoIterator<Item = &'a str>,
{
    let mut excerpts = Vec::new();
    let mut discarded = 0;
    for candidate in candidates {
        let candidate = candidate.trim();
        if candidate.is_empty() {
            continue;
        }
        if page.contains(candidate) {
            excerpts.push(candidate.to_string());
        } else {
            discarded += 1;
        }
    }
    ExtractedExcerpts {
        excerpts,
        discarded,
    }
}

pub fn parse_candidates(response: &str) -> Vec<String> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(response) {
        if let Some(items) = value.as_array() {
            return items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect();
        }
        if let Some(items) = value.get("excerpts").and_then(serde_json::Value::as_array) {
            return items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect();
        }
    }
    response
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            line.strip_prefix("- ")
                .or_else(|| line.strip_prefix("* "))
                .unwrap_or(line)
                .trim()
                .trim_matches('`')
                .to_string()
        })
        .collect()
}

pub async fn extract_prompt(
    prompt: &str,
    page: &str,
    model: Option<&str>,
    is_non_interactive_session: bool,
    abort_controller: &AbortController,
) -> anyhow::Result<ExtractedExcerpts> {
    if prompt.trim().is_empty() {
        return Ok(ExtractedExcerpts {
            excerpts: vec![page.to_string()],
            discarded: 0,
        });
    }
    if abort_controller.is_aborted() {
        anyhow::bail!("Operation aborted");
    }
    // @nyan: tests answer with a fixed reply instead of calling a model.
    #[cfg(test)]
    if let Some(reply) = test_reply::take() {
        return Ok(excerpts_from_reply(&reply, page));
    }

    let model = model
        .filter(|model| !model.is_empty())
        .map(str::to_string)
        .unwrap_or_else(crate::utils::model::model::get_main_loop_model);
    let instruction = format!(
        "Select the passages that answer the request. Return ONLY a JSON array of exact verbatim excerpts from the page, with no rewriting or commentary. If none answer, return [].\n\nRequest:\n{prompt}\n\nPage:\n{page}"
    );
    let user_message = crate::types::message::Message::User(crate::types::message::UserMessage {
        uuid: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now(),
        content: vec![crate::types::message::UserContent::Text(instruction)],
        is_compact_summary: false,
        plan_content: None,
        image_paste_ids: None,
        is_visible_in_transcript_only: false,
        mcp_meta: None,
        source_tool_assistant_uuid: None,
        permission_mode: None,
        origin: None,
        summarize_metadata: None,
    });
    let mut options = crate::services::api::request::Options::new(model, "web_extract".to_string());
    options.is_non_interactive_session = is_non_interactive_session;
    options.has_append_system_prompt = false;
    options.mcp_tools = Vec::new();
    options.abort_signal = Some(abort_controller.signal());
    let assistant = crate::services::api::request::query_model_without_streaming(
        &[user_message],
        &Vec::new(),
        &crate::utils::thinking::ThinkingConfig::Disabled,
        &[],
        &options,
    )
    .await?;
    if abort_controller.is_aborted() {
        anyhow::bail!("Operation aborted");
    }
    let response = assistant
        .content
        .into_iter()
        .filter_map(|block| match block {
            crate::types::message::AssistantContent::Text(text) => Some(text),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    Ok(excerpts_from_reply(&response, page))
}

/// The model's reply, reduced to the excerpts that occur verbatim in `page`.
pub fn excerpts_from_reply(reply: &str, page: &str) -> ExtractedExcerpts {
    validate_excerpts(parse_candidates(reply).iter().map(String::as_str), page)
}

#[cfg(test)]
pub(crate) mod test_reply {
    use std::sync::Mutex;

    static REPLY: Mutex<Option<String>> = Mutex::new(None);

    pub fn set(reply: &str) {
        *REPLY.lock().unwrap() = Some(reply.to_string());
    }

    pub fn take() -> Option<String> {
        REPLY.lock().unwrap().take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_verbatim_snapshot_excerpts_survive() {
        let result = validate_excerpts(
            ["The exact sentence", "model paraphrase", ""]
                .iter()
                .copied(),
            "Prefix. The exact sentence. Suffix.",
        );
        assert_eq!(result.excerpts, vec!["The exact sentence"]);
        assert_eq!(result.discarded, 1);
        assert!(result.render().contains("1 excerpts discarded"));
    }

    #[test]
    fn parses_json_and_plain_line_replies() {
        assert_eq!(
            parse_candidates(r#"["one", "two"]"#),
            vec!["one".to_string(), "two".to_string()]
        );
        assert_eq!(
            parse_candidates("- one\n* two\n"),
            vec!["one".to_string(), "two".to_string()]
        );
    }
}
