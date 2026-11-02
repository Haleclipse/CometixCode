//! Maps to: CC `components/FileEditToolUseRejectedMessage.tsx`.
//!
//! Renders rejected file write/update tool uses. Official branches are:
//! condensed text-only, new-file content preview, update diff preview, and
//! text-only fallback when no patch is available.

use crate::components::highlighted_code::HighlightedCode;
use crate::components::message_response::MessageResponse;
use crate::components::structured_diff_list::StructuredDiffList;
use crate::types::message::StructuredDiffHunk;
use iocraft::prelude::*;
use std::path::{Path, PathBuf};

const MAX_LINES_TO_RENDER: usize = 10;

#[derive(Default, Props)]
pub struct FileEditToolUseRejectedMessageProps {
    pub file_path: String,
    /// Official `operation: 'write' | 'update'`.
    pub operation: String,
    pub patch: Option<Vec<StructuredDiffHunk>>,
    pub first_line: Option<String>,
    pub file_content: Option<String>,
    pub content: Option<String>,
    pub style: Option<String>,
    pub verbose: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileEditRejectedContentPreview {
    pub truncated_content: String,
    pub hidden_lines: usize,
}

/// Maps to: CC `FileEditToolUseRejectedMessage`
/// `relative(getCwd(), file_path)`.
pub fn file_edit_rejected_display_path(file_path: &str, verbose: bool) -> String {
    if verbose {
        return file_path.to_string();
    }
    let cwd = crate::bootstrap::state::get_original_cwd();
    relative_path(&cwd, &PathBuf::from(file_path)).unwrap_or_else(|| file_path.to_string())
}

fn relative_path(from: &Path, to: &Path) -> Option<String> {
    let from = from.components().collect::<Vec<_>>();
    let to = to.components().collect::<Vec<_>>();
    if from.first() != to.first() {
        return None;
    }
    let common = from
        .iter()
        .zip(&to)
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative = PathBuf::new();
    for _ in common..from.len() {
        relative.push("..");
    }
    for component in &to[common..] {
        relative.push(component.as_os_str());
    }
    Some(if relative.as_os_str().is_empty() {
        String::new()
    } else {
        relative.display().to_string()
    })
}

/// Maps to: CC new-file preview truncation branch.
pub fn file_edit_rejected_content_preview(
    content: &str,
    verbose: bool,
) -> FileEditRejectedContentPreview {
    let lines = content.split('\n').collect::<Vec<_>>();
    let hidden_lines = lines.len().saturating_sub(MAX_LINES_TO_RENDER);
    let truncated_content = if verbose {
        content.to_string()
    } else {
        lines
            .iter()
            .take(MAX_LINES_TO_RENDER)
            .copied()
            .collect::<Vec<_>>()
            .join("\n")
    };
    FileEditRejectedContentPreview {
        truncated_content: if truncated_content.is_empty() {
            "(No content)".to_string()
        } else {
            truncated_content
        },
        hidden_lines: if verbose { 0 } else { hidden_lines },
    }
}

fn is_condensed(style: Option<&str>) -> bool {
    style == Some("condensed")
}

fn rejected_text(file_path: &str, operation: &str, verbose: bool) -> String {
    format!(
        "User rejected {operation} to {}",
        file_edit_rejected_display_path(file_path, verbose)
    )
}

/// Maps to: CC `components/FileEditToolUseRejectedMessage.tsx#FileEditToolUseRejectedMessage`.
#[component]
pub fn FileEditToolUseRejectedMessage(
    props: &FileEditToolUseRejectedMessageProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    let (columns, _) = hooks.use_terminal_size();
    let width = columns.saturating_sub(12).max(1) as usize;
    let heading = rejected_text(&props.file_path, &props.operation, props.verbose);

    if is_condensed(props.style.as_deref()) && !props.verbose {
        return element! {
            MessageResponse {
                Text(content: heading, color: theme.subtle, wrap: TextWrap::Wrap)
            }
        };
    }

    if props.operation == "write"
        && let Some(content) = props.content.as_ref()
    {
        let preview = file_edit_rejected_content_preview(content, props.verbose);
        return element! {
            MessageResponse {
                View(flex_direction: FlexDirection::Column) {
                    Text(content: heading, color: theme.subtle, wrap: TextWrap::Wrap)
                    HighlightedCode(
                        code: preview.truncated_content,
                        file_path: props.file_path.clone(),
                        width: Some(width),
                        dim: true,
                    )
                    #(if preview.hidden_lines > 0 {
                        Some(element! {
                            Text(
                                content: format!("… +{} lines", preview.hidden_lines),
                                color: theme.inactive,
                                dim: true,
                                wrap: TextWrap::NoWrap,
                            )
                        })
                    } else {
                        None
                    })
                }
            }
        };
    }

    let patch = props.patch.clone().unwrap_or_default();
    if patch.is_empty() {
        return element! {
            MessageResponse {
                Text(content: heading, color: theme.subtle, wrap: TextWrap::Wrap)
            }
        };
    }

    element! {
        MessageResponse {
            View(flex_direction: FlexDirection::Column) {
                Text(content: heading, color: theme.subtle, wrap: TextWrap::Wrap)
                StructuredDiffList(
                    hunks: patch,
                    dim: true,
                    width: width,
                    file_path: props.file_path.clone(),
                    first_line: props.first_line.clone(),
                    file_content: props.file_content.clone(),
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    fn hunk(lines: &[&str]) -> StructuredDiffHunk {
        StructuredDiffHunk {
            old_start: 1,
            old_lines: lines.iter().filter(|line| !line.starts_with('+')).count(),
            new_start: 1,
            new_lines: lines.iter().filter(|line| !line.starts_with('-')).count(),
            lines: lines.iter().map(|line| line.to_string()).collect(),
        }
    }

    #[test]
    fn file_edit_rejected_preview_truncates_new_file_content_like_official() {
        let content = (0..12)
            .map(|idx| format!("line {idx}"))
            .collect::<Vec<_>>()
            .join("\n");
        let preview = file_edit_rejected_content_preview(&content, false);
        assert!(preview.truncated_content.contains("line 0"));
        assert!(!preview.truncated_content.contains("line 11"));
        assert_eq!(preview.hidden_lines, 2);
        assert_eq!(
            file_edit_rejected_content_preview("", false).truncated_content,
            "(No content)"
        );
    }

    #[test]
    fn file_edit_rejected_message_renders_write_preview_and_hidden_count() {
        let content = (0..12)
            .map(|idx| format!("line {idx}"))
            .collect::<Vec<_>>()
            .join("\n");
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                FileEditToolUseRejectedMessage(
                    file_path: "src/lib.rs".to_string(),
                    operation: "write".to_string(),
                    content: Some(content),
                    verbose: false,
                )
            }
        }
        .render(Some(120))
        .to_string();

        assert!(
            text.contains("User rejected write to src/lib.rs"),
            "canvas=\n{text}"
        );
        assert!(text.contains("line 0"), "canvas=\n{text}");
        assert!(text.contains("… +2 lines"), "canvas=\n{text}");
    }

    #[test]
    fn file_edit_rejected_message_renders_update_diff_when_patch_present() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                // Renders StructuredDiffList, which reads
                // `settings.syntax_highlighting_disabled` — strict since P7.
                crate::state::app_state::AppStateProvider(
                    children: crate::state::app_state::ProviderChildren::new(|| element! {
                        FileEditToolUseRejectedMessage(
                            file_path: "src/lib.rs".to_string(),
                            operation: "update".to_string(),
                            patch: Some(vec![hunk(&["-old", "+new"])]),
                            verbose: true,
                        )
                    }.into_any()),
                )
            }
        }
        .render(Some(120))
        .to_string();

        assert!(
            text.contains("User rejected update to src/lib.rs"),
            "canvas=\n{text}"
        );
        assert!(text.contains("-old"), "canvas=\n{text}");
        assert!(text.contains("+new"), "canvas=\n{text}");
    }
}
