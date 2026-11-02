//! Regular prompt normalization.
//! Maps to official `utils/processUserInput/processTextPrompt.ts`: create the
//! visible user message and mark the turn as queryable, including pasted image
//! blocks when the submission carried any.

use super::ProcessUserInputBaseResult;
use crate::constants::query_source::QuerySource;
use crate::types::message::{RenderableMessage, UserContent};
use uuid::Uuid;

pub fn process_text_prompt(
    input: String,
    uuid: Option<String>,
    permission_mode: Option<crate::types::permissions::PermissionMode>,
    image_content_blocks: Vec<UserContent>,
    image_paste_ids: Vec<u32>,
    is_meta: bool,
) -> ProcessUserInputBaseResult {
    let id = uuid.unwrap_or_else(|| Uuid::new_v4().to_string());
    // CC `processTextPrompt.ts:66-88`: with pasted images this is still ONE
    // user message — text block first (omitted entirely when the text is
    // blank), then the image blocks, with `imagePasteIds` on the same
    // envelope. `normalizeMessages` splits it per block at render, so the
    // pasted-image rows come from that split rather than a second message.
    let mut message = if image_content_blocks.is_empty() {
        RenderableMessage::user(id, input)
    } else {
        let mut content = Vec::with_capacity(image_content_blocks.len() + 1);
        if !input.trim().is_empty() {
            content.push(UserContent::Text(input));
        }
        content.extend(image_content_blocks);
        RenderableMessage::user_blocks(id, content)
    };
    if is_meta
        && let crate::types::message::RenderableMessageKind::User { message } = &mut message.kind
    {
        for block in &mut message.content {
            *block = match std::mem::replace(block, UserContent::Text(String::new())) {
                UserContent::Text(text) => UserContent::MetaText(text),
                UserContent::Image { media_type, data } => {
                    UserContent::MetaImage { media_type, data }
                }
                other => other,
            };
        }
    }
    // CC `processTextPrompt(...)` threads `permissionMode` into
    // `createUserMessage` (`processTextPrompt.ts:75-93`) — recorded on the
    // envelope "for rewind restoration" (`REPL.tsx:4938-4947`). Wire string
    // form, matching CC's string-union `PermissionMode`.
    if let crate::types::message::RenderableMessageKind::User { message } = &mut message.kind {
        if let Some(mode) = permission_mode {
            message.permission_mode = Some(
                crate::utils::permissions::permission_mode::permission_mode_internal_name(mode)
                    .to_string(),
            );
        }
        // CC passes `imagePasteIds: ids.length > 0 ? ids : undefined`.
        if !image_paste_ids.is_empty() {
            message.image_paste_ids = Some(image_paste_ids);
        }
    }
    ProcessUserInputBaseResult {
        messages: vec![message],
        should_query: true,
        allowed_tools: None,
        local_action: None,
        query_source: QuerySource::Prompt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::{RenderableMessageKind, UserContent};

    /// Maps to: CC `processTextPrompt.ts:66-88` — pasted images become blocks
    /// of the ONE submitted user message, with `imagePasteIds` on the same
    /// envelope, so `normalizeMessages` produces the per-image rows by
    /// splitting it. Minting a separate message for them broke transcript
    /// structure, parent identity and the resumed image numbering.
    #[test]
    fn pasted_images_become_blocks_of_the_submitted_message() {
        let image = |data: &str| UserContent::Image {
            media_type: "image/png".to_string(),
            data: data.to_string(),
        };
        let result = process_text_prompt(
            "describe".to_string(),
            Some("prompt-uuid".to_string()),
            None,
            vec![image("AAAA"), image("BBBB")],
            vec![7, 9],
            false,
        );

        assert_eq!(result.messages.len(), 1, "one message, not one per image");
        let RenderableMessageKind::User { message } = &result.messages[0].kind else {
            panic!("expected a user message");
        };
        assert_eq!(
            result.messages[0].uuid, "prompt-uuid",
            "uuid is not re-minted"
        );
        assert!(matches!(
            message.content.as_slice(),
            [
                UserContent::Text(text),
                UserContent::Image { data: first, .. },
                UserContent::Image { data: second, .. },
            ] if text == "describe" && first == "AAAA" && second == "BBBB"
        ));
        assert_eq!(message.image_paste_ids.as_deref(), Some([7, 9].as_slice()));
    }

    /// CC omits the text block entirely when the prompt is blank
    /// (`processTextPrompt.ts:69-72`), and passes `imagePasteIds: undefined`
    /// rather than an empty array.
    #[test]
    fn blank_prompt_with_images_yields_image_blocks_only() {
        let result = process_text_prompt(
            "   ".to_string(),
            Some("prompt-uuid".to_string()),
            None,
            vec![UserContent::Image {
                media_type: "image/png".to_string(),
                data: "AAAA".to_string(),
            }],
            Vec::new(),
            false,
        );

        let RenderableMessageKind::User { message } = &result.messages[0].kind else {
            panic!("expected a user message");
        };
        assert!(matches!(
            message.content.as_slice(),
            [UserContent::Image { .. }]
        ));
        assert_eq!(message.image_paste_ids, None);
    }

    #[test]
    fn process_text_prompt_creates_user_message_and_should_query() {
        let result = process_text_prompt(
            "hello".to_string(),
            Some("u1".to_string()),
            None,
            Vec::new(),
            Vec::new(),
            false,
        );

        assert!(result.should_query);
        assert!(result.local_action.is_none());
        assert_eq!(result.messages.len(), 1);
        assert_eq!(result.messages[0].uuid, "u1");
        assert!(matches!(
            &result.messages[0].kind,
            RenderableMessageKind::User { message } if matches!(
                message.first_content_block(),
                Some(UserContent::Text(text)) if text == "hello"
            )
        ));
    }

    #[test]
    fn process_text_prompt_records_permission_mode_on_the_envelope() {
        // CC `createUserMessage({..., permissionMode})`
        // (`processTextPrompt.ts:89-93`).
        let result = process_text_prompt(
            "hello".to_string(),
            Some("u1".to_string()),
            Some(crate::types::permissions::PermissionMode::Plan),
            Vec::new(),
            Vec::new(),
            false,
        );

        assert!(matches!(
            &result.messages[0].kind,
            RenderableMessageKind::User { message }
                if message.permission_mode.as_deref() == Some("plan")
        ));
    }
}
