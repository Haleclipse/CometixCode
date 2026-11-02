//! Maps to: CC
//! `components/permissions/AskUserQuestionPermissionRequest/PreviewBox.tsx`.
//!
//! Official PreviewBox renders markdown/ANSI inside a bordered monospace box and
//! truncates with a hidden-lines separator. This Rust port keeps that visible
//! contract while leaving rich syntax highlighting to the broader Markdown slice.

use crate::utils::theme::Theme;
use iocraft::prelude::*;
use unicode_width::UnicodeWidthStr;

const TOP_LEFT: &str = "┌";
const TOP_RIGHT: &str = "┐";
const BOTTOM_LEFT: &str = "└";
const BOTTOM_RIGHT: &str = "┘";
const HORIZONTAL: &str = "─";
const VERTICAL: &str = "│";
const TEE_LEFT: &str = "├";
const TEE_RIGHT: &str = "┤";

#[derive(Default, Props)]
pub struct PreviewBoxProps {
    pub content: String,
    pub max_lines: Option<usize>,
    pub min_height: Option<usize>,
    pub min_width: Option<usize>,
    pub max_width: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewBoxLine {
    pub text: String,
    pub is_truncation: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewBoxRenderModel {
    pub width: usize,
    pub lines: Vec<PreviewBoxLine>,
}

fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn truncate_to_width(text: &str, width: usize) -> String {
    let mut result = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let ch_width = UnicodeWidthStr::width(ch.to_string().as_str());
        if used + ch_width > width {
            break;
        }
        result.push(ch);
        used += ch_width;
    }
    result
}

/// Maps to: CC `PreviewBoxBody` line measurement/truncation model.
pub fn preview_box_model(
    content: &str,
    max_lines: Option<usize>,
    min_height: Option<usize>,
    min_width: usize,
    max_width: usize,
) -> PreviewBoxRenderModel {
    let effective_max_lines = max_lines.unwrap_or(20).max(1);
    let content_lines = content.lines().collect::<Vec<_>>();
    let content_lines = if content_lines.is_empty() {
        vec![""]
    } else {
        content_lines
    };
    let is_truncated = content_lines.len() > effective_max_lines;
    let mut displayed = content_lines
        .iter()
        .take(effective_max_lines)
        .map(|line| (*line).to_string())
        .collect::<Vec<_>>();
    let effective_min_height = min_height.unwrap_or(0).min(effective_max_lines);
    let padding_needed = effective_min_height
        .saturating_sub(displayed.len() + if is_truncated { 1usize } else { 0usize });
    displayed.extend(std::iter::repeat_n(String::new(), padding_needed));

    let content_width = displayed
        .iter()
        .map(|line| display_width(line))
        .max()
        .unwrap_or(0)
        .max(min_width);
    let width = (content_width + 4).min(max_width.max(4));
    let inner_width = width.saturating_sub(4);

    let mut lines = Vec::new();
    lines.push(PreviewBoxLine {
        text: format!("{TOP_LEFT}{}{TOP_RIGHT}", HORIZONTAL.repeat(width - 2)),
        is_truncation: false,
    });

    for line in displayed {
        let display_line = if display_width(&line) > inner_width {
            truncate_to_width(&line, inner_width)
        } else {
            line
        };
        let padding = " ".repeat(inner_width.saturating_sub(display_width(&display_line)));
        lines.push(PreviewBoxLine {
            text: format!("{VERTICAL} {display_line}{padding} {VERTICAL}"),
            is_truncation: false,
        });
    }

    if is_truncated {
        let hidden_count = content_lines.len() - effective_max_lines;
        let label = format!(
            "{} ✂ {} {hidden_count} lines hidden ",
            HORIZONTAL.repeat(3),
            HORIZONTAL.repeat(3)
        );
        let fill_width = width.saturating_sub(2 + display_width(&label));
        lines.push(PreviewBoxLine {
            text: format!(
                "{TEE_LEFT}{label}{}{TEE_RIGHT}",
                HORIZONTAL.repeat(fill_width)
            ),
            is_truncation: true,
        });
    }

    lines.push(PreviewBoxLine {
        text: format!(
            "{BOTTOM_LEFT}{}{BOTTOM_RIGHT}",
            HORIZONTAL.repeat(width - 2)
        ),
        is_truncation: false,
    });

    PreviewBoxRenderModel { width, lines }
}

/// Maps to: CC `PreviewBox`.
#[component]
pub fn PreviewBox(props: &PreviewBoxProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let (terminal_width, _) = hooks.use_terminal_size();
    let max_width = props
        .max_width
        .unwrap_or_else(|| (terminal_width as usize).saturating_sub(4).max(4));
    let model = preview_box_model(
        &props.content,
        props.max_lines,
        props.min_height,
        props.min_width.unwrap_or(40),
        max_width,
    );

    element! {
        View(flex_direction: FlexDirection::Column) {
            #(model.lines.iter().map(|line| element! {
                Text(
                    content: line.text.clone(),
                    color: if line.is_truncation { Some(theme.warning) } else { Some(theme.inactive) },
                    wrap: TextWrap::NoWrap,
                )
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    #[test]
    fn preview_box_model_truncates_with_hidden_lines_separator() {
        let model = preview_box_model("a\nb\nc\nd", Some(2), None, 4, 30);
        let text = model
            .lines
            .iter()
            .map(|line| line.text.clone())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(text.contains("┌"));
        assert!(text.contains("a"));
        assert!(text.contains("2 lines hidden"));
        assert!(model.lines.iter().any(|line| line.is_truncation));
    }

    #[test]
    fn preview_box_renders_bordered_content() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                PreviewBox(content: "line one\nline two".to_string(), min_width: Some(10usize), max_width: Some(30usize))
            }
        }
        .render(Some(80))
        .to_string();

        assert!(text.contains("┌"), "canvas=\n{text}");
        assert!(text.contains("line one"), "canvas=\n{text}");
        assert!(text.contains("└"), "canvas=\n{text}");
    }
}
