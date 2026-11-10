//! UI-only port of official `AskUserQuestionTool` result messages.

use super::Output;
use crate::components::messages::user_tool_result_message::utils::{
    ToolRenderLine, ToolRenderTone,
};
use crate::constants::figures::BLACK_CIRCLE;

/// The Rust stand-in for CC's `outputSchema.safeParse(toolUseResult)`
/// (`UserToolSuccessMessage.tsx:80`): `questions` (array of questionSchema)
/// and `answers` (string→string record) are required, `annotations` optional
/// (`AskUserQuestionTool.tsx:151-163`).
pub(crate) fn parse_output(value: &serde_json::Value) -> Option<Output> {
    let map = value.as_object()?;

    // z.array(questionSchema): question/header strings, options 2-4 with
    // label/description strings (preview optional), multiSelect defaulted
    // boolean (AskUserQuestionTool.tsx:44-70).
    let questions = map.get("questions")?.as_array()?;
    for question in questions {
        let question = question.as_object()?;
        question.get("question")?.as_str()?;
        question.get("header")?.as_str()?;
        let options = question.get("options")?.as_array()?;
        if options.len() < 2 || options.len() > 4 {
            return None;
        }
        for option in options {
            let option = option.as_object()?;
            option.get("label")?.as_str()?;
            option.get("description")?.as_str()?;
            match option.get("preview") {
                None | Some(serde_json::Value::String(_)) => {}
                Some(_) => return None,
            }
        }
        match question.get("multiSelect") {
            None | Some(serde_json::Value::Bool(_)) => {}
            Some(_) => return None,
        }
    }

    let answers = map.get("answers")?.as_object()?;
    let mut parsed_answers = indexmap::IndexMap::new();
    for (question, answer) in answers {
        parsed_answers.insert(question.clone(), answer.as_str()?.to_string());
    }

    // annotationsSchema: optional record of {preview?, notes?} strings
    // (AskUserQuestionTool.tsx:72-92).
    let annotations = match map.get("annotations") {
        None => None,
        Some(value) => {
            let record = value.as_object()?;
            for annotation in record.values() {
                let annotation = annotation.as_object()?;
                for key in ["preview", "notes"] {
                    match annotation.get(key) {
                        None | Some(serde_json::Value::String(_)) => {}
                        Some(_) => return None,
                    }
                }
            }
            Some(value.clone())
        }
    };

    Some(Output {
        questions: questions.clone(),
        answers: parsed_answers,
        annotations,
    })
}

/// Serializes [`Output`] to CC's exact `toolUseResult` wire shape — the
/// `call()` construction (`AskUserQuestionTool.tsx:296-300`):
/// `{questions, answers, ...(annotations && {annotations})}`.
pub(crate) fn output_to_value(output: &Output) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert(
        "questions".to_string(),
        serde_json::Value::Array(output.questions.clone()),
    );
    let mut answers = serde_json::Map::new();
    for (question, answer) in &output.answers {
        answers.insert(question.clone(), serde_json::Value::String(answer.clone()));
    }
    map.insert("answers".to_string(), serde_json::Value::Object(answers));
    if let Some(annotations) = output.annotations.as_ref() {
        map.insert("annotations".to_string(), annotations.clone());
    }
    serde_json::Value::Object(map)
}

/// Maps to: CC `AskUserQuestionTool.tsx:175-197`
/// `AskUserQuestionResultMessage` — the "User answered nyan's questions:"
/// header (always with the colon) plus one `· {question} → {answer}` line per
/// entry, iterated in the record's insertion order.
pub(crate) fn render_tool_result_message(
    raw_output: Option<&serde_json::Value>,
) -> Vec<ToolRenderLine> {
    let Some(output) = raw_output.and_then(parse_output) else {
        return Vec::new();
    };
    let mut lines = vec![ToolRenderLine::new(
        format!("{BLACK_CIRCLE} User answered nyan's questions:"),
        ToolRenderTone::Normal,
    )];
    lines.extend(output.answers.iter().map(|(question, answer)| {
        ToolRenderLine::new(format!("· {question} → {answer}"), ToolRenderTone::Inactive)
    }));
    lines
}

/// Maps to: CC `AskUserQuestionTool.tsx:285-292` `renderToolUseRejectedMessage`.
pub fn render_rejected_result_lines() -> Vec<ToolRenderLine> {
    vec![ToolRenderLine::new(
        format!("{BLACK_CIRCLE} User declined to answer questions"),
        ToolRenderTone::Normal,
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn raw() -> serde_json::Value {
        json!({
            "questions": [{
                "question": "Which auth method?",
                "header": "Auth method",
                "options": [
                    {"label": "OAuth", "description": "Standard OAuth flow"},
                    {"label": "API key", "description": "Static key"},
                ],
                "multiSelect": false,
            }],
            "answers": {"Which auth method?": "OAuth"},
        })
    }

    #[test]
    fn ask_user_question_result_renders_header_and_answers() {
        let lines = render_tool_result_message(Some(&raw()));
        // CC's header always carries the colon, even with zero answers.
        assert_eq!(
            lines[0].text,
            format!("{BLACK_CIRCLE} User answered nyan's questions:")
        );
        assert_eq!(lines[1].text, "· Which auth method? → OAuth");
        assert_eq!(lines[1].tone, ToolRenderTone::Inactive);
    }

    #[test]
    fn ask_user_question_parse_enforces_schema_and_round_trips() {
        // A non-string answer fails the record value schema.
        let mut bad = raw();
        bad["answers"]["Which auth method?"] = json!(42);
        assert!(parse_output(&bad).is_none());

        // options must be 2-4.
        let mut one_option = raw();
        one_option["questions"][0]["options"] = json!([
            {"label": "OAuth", "description": "Standard OAuth flow"},
        ]);
        assert!(parse_output(&one_option).is_none());

        let output = parse_output(&raw()).unwrap();
        assert_eq!(output_to_value(&output), raw());
    }
}
