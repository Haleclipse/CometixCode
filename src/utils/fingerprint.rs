//! Maps to: CC `utils/fingerprint.ts`.
//!
//! 3-character fingerprint for Claude Code attribution.
//!
//! IMPORTANT (CC): Do not change this method without careful coordination with
//! 1P and 3P (Bedrock, Vertex, Azure) APIs.

use crate::types::message::{Message, UserContent};
use sha2::{Digest, Sha256};

/// Hardcoded salt from backend validation.
/// Must match exactly for fingerprint validation to pass.
/// Maps to: CC `FINGERPRINT_SALT`.
pub const FINGERPRINT_SALT: &str = "59cf53e54c78";

/// Extracts text content from the first user message.
///
/// Returns the first `text` block of the first `user` message, or an empty
/// string if there is none (image-only / tool_result-only first message).
///
/// Maps to: CC `utils/fingerprint.ts:16-38` `extractFirstMessageText(messages)`.
pub fn extract_first_message_text(messages: &[Message]) -> String {
    let Some(first_user_message) = messages.iter().find_map(|message| match message {
        Message::User(user) => Some(user),
        _ => None,
    }) else {
        return String::new();
    };
    // CC: `content.find(block => block.type === 'text')` — both plain and
    // isMeta text carriers serialize as `text` blocks on the wire.
    first_user_message
        .content
        .iter()
        .find_map(|block| match block {
            UserContent::Text(text) | UserContent::MetaText(text) => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// Computes 3-character fingerprint for Claude Code attribution.
///
/// Algorithm: `SHA256(SALT + msg[4] + msg[7] + msg[20] + version)[:3]`
///
/// - Extract chars at indices `[4, 7, 20]`, use `"0"` if index not found
/// - SHA256 hash, return first 3 hex chars
///
/// Maps to: CC `computeFingerprint(messageText, version)`.
pub fn compute_fingerprint(message_text: &str, version: &str) -> String {
    // Extract chars at indices [4, 7, 20], use "0" if index not found.
    let indices = [4usize, 7, 20];
    let chars: String = indices
        .iter()
        .map(|&i| message_text.chars().nth(i).unwrap_or('0'))
        .collect();
    let fingerprint_input = format!("{FINGERPRINT_SALT}{chars}{version}");
    // SHA256 hash, return first 3 hex chars.
    let hash = Sha256::digest(fingerprint_input.as_bytes());
    hash.iter()
        .take(2)
        .fold(String::new(), |mut acc, b| {
            // 2 bytes → 4 hex chars, take first 3
            use std::fmt::Write;
            let _ = write!(acc, "{b:02x}");
            acc
        })
        .chars()
        .take(3)
        .collect()
}

/// Computes fingerprint from the first user message.
///
/// Maps to: CC `utils/fingerprint.ts:71-76` `computeFingerprintFromMessages(messages)`
/// — `computeFingerprint(extractFirstMessageText(messages), MACRO.VERSION)`.
pub fn compute_fingerprint_from_messages(messages: &[Message]) -> String {
    let first_message_text = extract_first_message_text(messages);
    compute_fingerprint(&first_message_text, crate::constants::product::VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(content: Vec<UserContent>) -> Message {
        let mut message = crate::utils::messages::create_user_message(String::new());
        message.content = content;
        Message::User(message)
    }

    #[test]
    fn extract_first_message_text_matches_official_first_user_text_block() {
        let messages = vec![
            Message::Assistant(crate::utils::messages::create_assistant_message(
                "assistant first".to_string(),
            )),
            user(vec![
                UserContent::Image {
                    media_type: "image/png".to_string(),
                    data: "abc".to_string(),
                },
                UserContent::Text("hello world".to_string()),
                UserContent::Text("second block".to_string()),
            ]),
            user(vec![UserContent::Text("later".to_string())]),
        ];
        assert_eq!(extract_first_message_text(&messages), "hello world");
        assert_eq!(extract_first_message_text(&[]), "");
        assert_eq!(
            extract_first_message_text(&[user(vec![UserContent::Image {
                media_type: "image/png".to_string(),
                data: "abc".to_string(),
            }])]),
            ""
        );
    }

    #[test]
    fn compute_fingerprint_from_messages_uses_first_user_text_and_build_version() {
        let messages = vec![Message::User(crate::utils::messages::create_user_message(
            "hello world from nyan".to_string(),
        ))];
        assert_eq!(
            compute_fingerprint_from_messages(&messages),
            compute_fingerprint("hello world from nyan", crate::constants::product::VERSION)
        );
        // No user text → CC fingerprints the empty string ("0" at every index).
        assert_eq!(
            compute_fingerprint_from_messages(&[]),
            compute_fingerprint("", crate::constants::product::VERSION)
        );
    }

    #[test]
    fn compute_fingerprint_is_three_hex_chars_and_stable() {
        let a = compute_fingerprint("hello world from nyan", "1.0.0");
        let b = compute_fingerprint("hello world from nyan", "1.0.0");
        assert_eq!(a.len(), 3);
        assert_eq!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
