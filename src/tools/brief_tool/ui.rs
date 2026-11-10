//! Maps to: CC `tools/BriefTool/UI.tsx` (complete file).

use crate::components::markdown::Markdown;
use crate::constants::figures::{BLACK_CIRCLE, figures};
use crate::tools::brief_tool::{BriefAttachmentOutput, BriefOutput};
use crate::utils::file::get_display_path;
use crate::utils::format::format_file_size;
use crate::utils::format_brief_timestamp::format_brief_timestamp;
use crate::utils::theme::Theme;
use chrono::Local;
use iocraft::prelude::*;

/// Maps to: CC `tools/BriefTool/UI.tsx:12-14` `renderToolUseMessage`.
pub fn render_tool_use_message() -> &'static str {
    ""
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BriefResultLayout {
    Transcript,
    BriefOnly,
    #[default]
    Default,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BriefRenderedMessage {
    pub output: BriefOutput,
    pub layout: BriefResultLayout,
    pub timestamp: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BriefRenderOptions {
    pub is_transcript_mode: bool,
    pub is_brief_only: bool,
}

/// The Rust stand-in for CC's `outputSchema.safeParse(toolUseResult)`
/// (`UserToolSuccessMessage.tsx:80`): `message` is required, `attachments`
/// (each with required path/size/isImage, optional file_uuid) and `sentAt`
/// optional (`BriefTool.ts:42-63`). `size` is an unconstrained `z.number()`;
/// the u64 carrier narrows fractional sizes, which fs sizes never are.
pub(crate) fn parse_output(value: &serde_json::Value) -> Option<BriefOutput> {
    let map = value.as_object()?;
    let optional_string = |key: &str| -> Option<Option<String>> {
        match map.get(key) {
            None => Some(None),
            Some(serde_json::Value::String(value)) => Some(Some(value.clone())),
            Some(_) => None,
        }
    };
    let attachments = match map.get("attachments") {
        None => None,
        Some(serde_json::Value::Array(items)) => Some(
            items
                .iter()
                .map(|item| {
                    let item = item.as_object()?;
                    let file_uuid = match item.get("file_uuid") {
                        None => None,
                        Some(serde_json::Value::String(uuid)) => Some(uuid.clone()),
                        Some(_) => return None,
                    };
                    Some(BriefAttachmentOutput {
                        path: item.get("path")?.as_str()?.to_string(),
                        size: item.get("size")?.as_u64()?,
                        is_image: item.get("isImage")?.as_bool()?,
                        file_uuid,
                    })
                })
                .collect::<Option<Vec<_>>>()?,
        ),
        Some(_) => return None,
    };
    Some(BriefOutput {
        message: map.get("message")?.as_str()?.to_string(),
        attachments,
        sent_at: optional_string("sentAt")?,
    })
}

/// Serializes [`BriefOutput`] to CC's exact `toolUseResult` wire shape — the
/// `call()` construction (`BriefTool.ts:193,201`), optionals omitted.
pub(crate) fn output_to_value(output: &BriefOutput) -> serde_json::Value {
    serde_json::to_value(output).unwrap_or(serde_json::Value::Null)
}

/// Maps to: CC `tools/BriefTool/UI.tsx:16-78` `renderToolResultMessage`.
/// L1 (`React/Ink -> iocraft`): the returned value is retained render data;
/// `BriefToolResultMessage` below performs only the equivalent element projection.
pub fn render_tool_result_message(
    output: &BriefOutput,
    options: BriefRenderOptions,
) -> Option<BriefRenderedMessage> {
    let has_attachments = output
        .attachments
        .as_deref()
        .is_some_and(|attachments| !attachments.is_empty());
    if output.message.is_empty() && !has_attachments {
        return None;
    }

    let layout = if options.is_transcript_mode {
        BriefResultLayout::Transcript
    } else if options.is_brief_only {
        BriefResultLayout::BriefOnly
    } else {
        BriefResultLayout::Default
    };
    let timestamp = if layout == BriefResultLayout::BriefOnly {
        output
            .sent_at
            .as_deref()
            .map(|sent_at| format_brief_timestamp(sent_at, Local::now()))
            .unwrap_or_default()
    } else {
        String::new()
    };

    Some(BriefRenderedMessage {
        output: output.clone(),
        layout,
        timestamp,
    })
}

#[derive(Clone, Debug, Default, Props)]
pub struct AttachmentListProps {
    pub attachments: Vec<BriefAttachmentOutput>,
}

/// Maps to: CC `tools/BriefTool/UI.tsx:81-105` `AttachmentList`.
#[component]
pub fn AttachmentList(props: &AttachmentListProps) -> impl Into<AnyElement<'static>> {
    if props.attachments.is_empty() {
        return element! { View(width: 0u32, height: 0u32) }.into_any();
    }

    element! {
        View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
            #(props.attachments.iter().map(|attachment| {
                let kind = if attachment.is_image { "[image]" } else { "[file]" };
                element! {
                    View(
                        key: attachment.path.clone(),
                        flex_direction: FlexDirection::Row,
                    ) {
                        Text(
                            content: format!("{} {kind} ", figures().pointer_small),
                            dim: true,
                            wrap: TextWrap::NoWrap,
                        )
                        Text(
                            content: get_display_path(&attachment.path),
                            wrap: TextWrap::NoWrap,
                        )
                        Text(
                            content: format!(" ({})", format_file_size(attachment.size)),
                            dim: true,
                            wrap: TextWrap::NoWrap,
                        )
                    }
                }
            }))
        }
    }
    .into_any()
}

#[derive(Clone, Debug, Default, Props)]
pub struct BriefToolResultMessageProps {
    pub rendered: BriefRenderedMessage,
}

/// L1 (`React/Ink -> iocraft`) element projection for
/// `tools/BriefTool/UI.tsx:16-78` `renderToolResultMessage`.
#[component]
pub fn BriefToolResultMessage(
    props: &BriefToolResultMessageProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let message =
        (!props.rendered.output.message.is_empty()).then(|| props.rendered.output.message.clone());
    let attachments = props
        .rendered
        .output
        .attachments
        .clone()
        .unwrap_or_default();

    match props.rendered.layout {
        BriefResultLayout::Transcript => element! {
            View(flex_direction: FlexDirection::Row, margin_top: 1u32) {
                View(min_width: 2u32, flex_shrink: 0.0f32) {
                    Text(content: BLACK_CIRCLE.to_string(), color: theme.text, wrap: TextWrap::NoWrap)
                }
                View(flex_direction: FlexDirection::Column, flex_grow: 1.0f32) {
                    #(message.clone().map(|content| element! { Markdown(content) }))
                    AttachmentList(attachments: attachments.clone())
                }
            }
        }
        .into_any(),
        BriefResultLayout::BriefOnly => element! {
            View(flex_direction: FlexDirection::Column, margin_top: 1u32, padding_left: 2u32) {
                View(flex_direction: FlexDirection::Row) {
                    Text(
                        content: "nyan".to_string(),
                        color: theme.brief_label_claude,
                        wrap: TextWrap::NoWrap,
                    )
                    #(if props.rendered.timestamp.is_empty() {
                        None
                    } else {
                        Some(element! {
                            Text(
                                content: format!(" {}", props.rendered.timestamp),
                                dim: true,
                                wrap: TextWrap::NoWrap,
                            )
                        })
                    })
                }
                View(flex_direction: FlexDirection::Column) {
                    #(message.clone().map(|content| element! { Markdown(content) }))
                    AttachmentList(attachments: attachments.clone())
                }
            }
        }
        .into_any(),
        BriefResultLayout::Default => element! {
            View(flex_direction: FlexDirection::Row, margin_top: 1u32) {
                View(min_width: 2u32, flex_shrink: 0.0f32)
                View(flex_direction: FlexDirection::Column, flex_grow: 1.0f32) {
                    #(message.map(|content| element! { Markdown(content) }))
                    AttachmentList(attachments)
                }
            }
        }
        .into_any(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output() -> BriefOutput {
        BriefOutput {
            message: "Hello **there**".to_string(),
            attachments: Some(vec![BriefAttachmentOutput {
                path: "/tmp/report.txt".to_string(),
                size: 2048,
                is_image: false,
                file_uuid: None,
            }]),
            sent_at: None,
        }
    }

    fn render(rendered: BriefRenderedMessage) -> String {
        element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                BriefToolResultMessage(rendered)
            }
        }
        .render(Some(80))
        .to_string()
    }

    #[test]
    fn brief_tool_ui_matches_official_hidden_use_and_default_result() {
        assert_eq!(render_tool_use_message(), "");
        let rendered = render_tool_result_message(&output(), BriefRenderOptions::default())
            .expect("brief result");
        assert_eq!(rendered.layout, BriefResultLayout::Default);
        let canvas = render(rendered);
        assert!(canvas.contains("Hello there"), "canvas=\n{canvas}");
        assert!(
            canvas.contains("› [file] /tmp/report.txt (2KB)"),
            "canvas=\n{canvas}"
        );
        assert!(!canvas.contains(BLACK_CIRCLE), "canvas=\n{canvas}");
        assert!(!canvas.contains("nyan"), "canvas=\n{canvas}");
    }

    #[test]
    fn brief_tool_result_branches_match_official_transcript_and_brief_only_ui() {
        let transcript = render_tool_result_message(
            &output(),
            BriefRenderOptions {
                is_transcript_mode: true,
                is_brief_only: true,
            },
        )
        .expect("transcript result");
        assert_eq!(transcript.layout, BriefResultLayout::Transcript);
        assert!(render(transcript).contains(BLACK_CIRCLE));

        let brief_only = render_tool_result_message(
            &output(),
            BriefRenderOptions {
                is_transcript_mode: false,
                is_brief_only: true,
            },
        )
        .expect("brief-only result");
        assert_eq!(brief_only.layout, BriefResultLayout::BriefOnly);
        assert!(render(brief_only).contains("nyan"));
    }

    #[test]
    fn brief_tool_result_empty_output_matches_official_null_branch() {
        let output = BriefOutput {
            message: String::new(),
            attachments: None,
            sent_at: None,
        };
        assert!(render_tool_result_message(&output, BriefRenderOptions::default()).is_none());
    }
}
