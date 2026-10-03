//! Maps to: CC `tools/BashTool/utils.ts`.
//!
//! Pure Bash output helpers, including image resizing/downsampling and
//! deterministic parsing/truncation/summary behavior.

use crate::utils::shell::output_limits::get_max_output_length;
use crate::utils::string_utils::{count_char_in_string, plural};
use regex::Regex;
use std::sync::LazyLock;

pub use crate::utils::shell::output_limits::{
    BASH_MAX_OUTPUT_DEFAULT, BASH_MAX_OUTPUT_UPPER_LIMIT,
};

static DATA_URI_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^data:([^;]+);base64,(.+)$").expect("valid data-uri regex"));
static IMAGE_OUTPUT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^data:image/[a-z0-9.+_-]+;base64,").expect("valid image output regex")
});

/// Maps to CC `stripEmptyLines(content)`.
pub fn strip_empty_lines(content: &str) -> String {
    let lines = content.lines().collect::<Vec<_>>();
    let mut start = 0usize;
    while start < lines.len() && lines[start].trim().is_empty() {
        start += 1;
    }
    let mut end = lines.len();
    while end > start && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    if start >= end {
        String::new()
    } else {
        lines[start..end].join("\n")
    }
}

/// Maps to CC `isImageOutput(content)`.
pub fn is_image_output(content: &str) -> bool {
    IMAGE_OUTPUT_RE.is_match(content)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataUriParts {
    pub media_type: String,
    pub data: String,
}

/// Maps to CC `parseDataUri(s)`.
pub fn parse_data_uri(value: &str) -> Option<DataUriParts> {
    let captures = DATA_URI_RE.captures(value.trim())?;
    Some(DataUriParts {
        media_type: captures.get(1)?.as_str().to_string(),
        data: captures.get(2)?.as_str().to_string(),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageToolResultBlock {
    pub tool_use_id: String,
    pub media_type: String,
    pub data: String,
}

/// Maps to CC `buildImageToolResult(stdout, toolUseID)` as a Rust data shape.
pub fn build_image_tool_result(stdout: &str, tool_use_id: &str) -> Option<ImageToolResultBlock> {
    let parsed = parse_data_uri(stdout)?;
    Some(ImageToolResultBlock {
        tool_use_id: tool_use_id.to_string(),
        media_type: parsed.media_type,
        data: parsed.data,
    })
}

/// Maps to CC `resizeShellImageOutput(stdout, outputFilePath, outputFileSize)`.
pub fn resize_shell_image_output(
    stdout: &str,
    output_file_path: Option<&str>,
    output_file_size: Option<u64>,
) -> Option<String> {
    use base64::Engine as _;
    const MAX_IMAGE_FILE_SIZE: u64 = 20 * 1024 * 1024;

    let source = if let Some(path) = output_file_path {
        if output_file_size.is_some_and(|size| size > MAX_IMAGE_FILE_SIZE) {
            return None;
        }
        let result = futures::executor::block_on(crate::utils::fs_operations::read_file_range(
            std::path::Path::new(path),
            0,
            (MAX_IMAGE_FILE_SIZE + 1) as usize,
        ))
        .ok()??;
        if result.bytes_total > MAX_IMAGE_FILE_SIZE {
            return None;
        }
        result.content
    } else {
        stdout.to_string()
    };
    let parsed = parse_data_uri(&source)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(parsed.data.as_bytes())
        .ok()?;
    let extension = parsed
        .media_type
        .split_once('/')
        .map(|(_, extension)| extension)
        .unwrap_or("png");
    let resized = crate::utils::image_resizer::maybe_resize_and_downsample_image_buffer(
        &bytes,
        bytes.len(),
        extension,
    )
    .ok()?;
    Some(format!(
        "data:{};base64,{}",
        resized.media_type,
        base64::engine::general_purpose::STANDARD.encode(resized.buffer)
    ))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormattedOutput {
    pub total_lines: usize,
    pub truncated_content: String,
    pub is_image: bool,
}

/// Maps to CC `formatOutput(content)`.
pub fn format_output(content: &str) -> FormattedOutput {
    let is_image = is_image_output(content);
    if is_image {
        return FormattedOutput {
            total_lines: 1,
            truncated_content: content.to_string(),
            is_image,
        };
    }

    let max_output_length = get_max_output_length();
    if content.encode_utf16().count() <= max_output_length {
        return FormattedOutput {
            total_lines: count_char_in_string(content, "\n", 0) + 1,
            truncated_content: content.to_string(),
            is_image,
        };
    }

    let (truncated_part, _) = split_at_utf16_units(content, max_output_length);
    let remaining_lines = count_char_in_string(content, "\n", max_output_length) + 1;
    FormattedOutput {
        total_lines: count_char_in_string(content, "\n", 0) + 1,
        truncated_content: format!(
            "{truncated_part}\n\n... [{remaining_lines} lines truncated] ..."
        ),
        is_image,
    }
}

/// Maps to CC `resetCwdIfOutsideProject(toolPermissionContext)`.
/// Rust receives the shell trailer's physical cwd explicitly and returns the
/// query-owned cwd effect instead of mutating process-global cwd in this helper.
pub(crate) fn reset_cwd_if_outside_project(
    cwd_after: Option<std::path::PathBuf>,
    context: &crate::tool::ToolUseContext,
) -> (Option<std::path::PathBuf>, bool) {
    let Some(cwd_after) = cwd_after else {
        return (None, false);
    };
    let original = crate::bootstrap::state::get_original_cwd();
    let maintain = crate::utils::env_utils::should_maintain_project_working_dir();
    let outside = cwd_after != original
        && !crate::utils::permissions::filesystem::path_in_allowed_working_path(
            &cwd_after.display().to_string(),
            &context.tool_permission_context,
            None,
        );
    if maintain || outside {
        (Some(original), outside && !maintain)
    } else {
        (Some(cwd_after), false)
    }
}

/// Maps to CC `stdErrAppendShellResetMessage(stderr)`.
pub fn stderr_append_shell_reset_message(stderr: &str) -> String {
    format!(
        "{}\nShell cwd was reset to {}",
        stderr.trim(),
        crate::bootstrap::state::get_original_cwd().display()
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentSummaryBlock {
    Text(String),
    Image,
}

/// Maps to CC `createContentSummary(content)`.
pub fn create_content_summary(content: &[ContentSummaryBlock]) -> String {
    let mut parts = Vec::new();
    let mut text_count = 0usize;
    let mut image_count = 0usize;

    for block in content {
        match block {
            ContentSummaryBlock::Image => image_count += 1,
            ContentSummaryBlock::Text(text) => {
                text_count += 1;
                let preview = text.chars().take(200).collect::<String>();
                parts.push(if text.chars().count() > 200 {
                    format!("{preview}...")
                } else {
                    preview
                });
            }
        }
    }

    let mut summary = Vec::new();
    if image_count > 0 {
        summary.push(format!(
            "[{image_count} {}]",
            plural(image_count, "image", None)
        ));
    }
    if text_count > 0 {
        summary.push(format!(
            "[{text_count} text {}]",
            plural(text_count, "block", None)
        ));
    }

    format!(
        "MCP Result: {}{}",
        summary.join(", "),
        if parts.is_empty() {
            String::new()
        } else {
            format!("\n\n{}", parts.join("\n\n"))
        }
    )
}

fn split_at_utf16_units(value: &str, max_units: usize) -> (&str, &str) {
    let mut units = 0usize;
    for (index, character) in value.char_indices() {
        let next = units + character.len_utf16();
        if next > max_units {
            return value.split_at(index);
        }
        units = next;
    }
    (value, "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn strip_empty_lines_preserves_internal_whitespace() {
        assert_eq!(strip_empty_lines("\n  \n  a\n\n  b  \n\n"), "  a\n\n  b  ");
        assert_eq!(strip_empty_lines("\n \n"), "");
    }

    #[test]
    fn data_uri_and_image_helpers_match_official_shapes() {
        let data_uri = "data:image/png;base64,AAAA";
        assert!(is_image_output(data_uri));
        assert_eq!(
            parse_data_uri(data_uri),
            Some(DataUriParts {
                media_type: "image/png".to_string(),
                data: "AAAA".to_string(),
            })
        );
        assert_eq!(
            build_image_tool_result(data_uri, "toolu_1"),
            Some(ImageToolResultBlock {
                tool_use_id: "toolu_1".to_string(),
                media_type: "image/png".to_string(),
                data: "AAAA".to_string(),
            })
        );
    }

    #[test]
    fn resize_shell_image_output_reads_complete_artifact_and_reencodes_data_uri() {
        use base64::Engine as _;
        use image::ImageEncoder as _;

        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&[255, 0, 0, 255], 1, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        let uri = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&png)
        );
        let path = std::env::temp_dir().join(format!(
            "cometix-bash-image-output-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&path, &uri).unwrap();
        let resized = resize_shell_image_output(
            &uri[..uri.len().min(24)],
            Some(&path.display().to_string()),
            Some(uri.len() as u64),
        )
        .expect("complete output artifact supplies valid image data");
        let parsed = parse_data_uri(&resized).unwrap();
        assert_eq!(parsed.media_type, "image/png");
        assert!(
            base64::engine::general_purpose::STANDARD
                .decode(parsed.data)
                .is_ok()
        );
        let _ = std::fs::remove_file(path);
    }

    #[cfg(unix)]
    #[test]
    fn resize_shell_image_output_follows_artifact_symlink_like_official() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "cometix-bash-image-symlink-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let victim = root.join("victim");
        let output = root.join("output");
        std::fs::write(&victim, "data:image/png;base64,AAAA").unwrap();
        symlink(&victim, &output).unwrap();
        assert!(
            resize_shell_image_output(
                "data:image/png;base64,AAAA",
                Some(&output.display().to_string()),
                Some(26),
            )
            .is_some()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn resize_shell_image_output_rejects_artifact_over_twenty_megabytes() {
        let path = std::env::temp_dir().join(format!(
            "cometix-bash-image-too-large-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(20 * 1024 * 1024 + 1).unwrap();
        assert!(
            resize_shell_image_output(
                "data:image/png;base64,AAAA",
                Some(&path.display().to_string()),
                Some(20 * 1024 * 1024 + 1),
            )
            .is_none()
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn format_output_truncates_with_official_suffix() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _env = EnvVarGuard::set("BASH_MAX_OUTPUT_LENGTH", "5");
        let formatted = format_output("12345\n678\n9");
        assert_eq!(formatted.total_lines, 3);
        assert_eq!(
            formatted.truncated_content,
            "12345\n\n... [3 lines truncated] ..."
        );
    }

    #[test]
    fn content_summary_counts_images_and_text_blocks() {
        assert_eq!(
            create_content_summary(&[
                ContentSummaryBlock::Image,
                ContentSummaryBlock::Text("hello".to_string()),
            ]),
            "MCP Result: [1 image], [1 text block]\n\nhello"
        );
    }
}
