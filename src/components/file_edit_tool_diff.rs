//! Maps to: CC `components/FileEditToolDiff.tsx`.
//!
//! Snapshots file content, computes the display patch, and renders retained
//! `StructuredDiffList` children inside the official dashed frame. It may read
//! the target file for display context but never writes it.

use crate::components::structured_diff_list::StructuredDiffList;
use crate::types::message::StructuredDiffHunk;
use crate::utils::diff::{DisplayEdit, get_patch_for_display};
use iocraft::prelude::*;

/// Maps to: CC `tools/FileEditTool/types.ts#FileEdit`.
/// Re-exported from the Edit tool types module (execution + display share it).
pub use crate::tools::file_edit_tool::types::FileEdit;

pub use crate::tools::file_edit_tool::utils::{
    find_actual_string, normalize_quotes, preserve_quote_style,
};

#[derive(Default, Props)]
pub struct FileEditToolDiffProps {
    pub file_path: String,
    pub edits: Vec<FileEdit>,
}

/// Maps to: CC `components/FileEditToolDiff.tsx#DiffData`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffData {
    pub patch: Vec<StructuredDiffHunk>,
    pub first_line: Option<String>,
    pub file_content: Option<String>,
}

/// Maps to: CC `components/FileEditToolDiff.tsx#FileEditToolDiff`.
#[component]
pub fn FileEditToolDiff(
    props: &FileEditToolDiffProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let (columns, _) = hooks.use_terminal_size();
    let file_path = props.file_path.clone();
    let edits = props.edits.clone();
    let mut data = hooks.use_state(|| Option::<DiffData>::None);
    let receiver = hooks.use_const(move || {
        let (sender, receiver) = async_channel::bounded(1);
        let _ = std::thread::Builder::new()
            .name("edit-permission-diff".to_string())
            .spawn(move || {
                let snapshot = load_diff_data(&file_path, &edits);
                let _ = sender.send_blocking(snapshot);
            });
        std::sync::Arc::new(receiver)
    });
    hooks.use_future(async move {
        if let Ok(snapshot) = receiver.recv().await {
            data.set(Some(snapshot));
        }
    });
    let data = data.read().clone();
    let theme = hooks.use_context::<crate::utils::theme::Theme>();

    element! {
        View(flex_direction: FlexDirection::Column) {
            // Maps to CC `DiffFrame` dashed border box and Suspense fallback.
            View(
                border_style: BorderStyle::Dashed,
                border_color: theme.subtle,
                border_left: false,
                border_right: false,
                flex_direction: FlexDirection::Column,
            ) {
                #(if let Some(data) = data {
                    Some(element! {
                        StructuredDiffList(
                            hunks: data.patch,
                            dim: false,
                            width: columns as usize,
                            file_path: props.file_path.clone(),
                            first_line: data.first_line,
                            file_content: data.file_content,
                        )
                    }.into_any())
                } else {
                    Some(element! { Text(content: "…".to_string(), dim: true) }.into_any())
                })
            }
        }
    }
}

/// Maps to: CC `components/FileEditToolDiff.tsx#loadDiffData`.
pub fn load_diff_data(file_path: &str, edits: &[FileEdit]) -> DiffData {
    // CC filters nullable fields. Rust's validated FileEdit fields are present.
    let valid = edits.to_vec();
    let single = (valid.len() == 1).then(|| &valid[0]);
    if single.is_some_and(|edit| {
        edit.old_string.encode_utf16().count() >= crate::utils::read_edit_context::CHUNK_SIZE
    }) {
        return diff_tool_inputs_only(file_path, &valid);
    }

    let path = std::path::Path::new(file_path);
    let mut file = match crate::utils::read_edit_context::open_for_scan(path) {
        Ok(Some(file)) => file,
        Ok(None) => return diff_tool_inputs_only(file_path, &valid),
        Err(error) => {
            crate::utils::debug::log_for_debugging(&format!(
                "Error opening file for edit diff: {error}"
            ));
            return diff_tool_inputs_only(file_path, &valid);
        }
    };

    if let Some(edit) = single.filter(|edit| !edit.old_string.is_empty()) {
        match crate::utils::read_edit_context::scan_for_context(&mut file, &edit.old_string, 4) {
            Ok(context) if !context.content.is_empty() => {
                let normalized = normalize_edit(&context.content, edit);
                let local_patch = get_patch_for_display(
                    &context.content,
                    &display_edits(std::slice::from_ref(&normalized)),
                );
                let first_line = (context.line_offset == 1).then(|| {
                    context
                        .content
                        .split_once('\n')
                        .map_or(context.content.as_str(), |(first, _)| first)
                        .to_string()
                });
                return DiffData {
                    patch: crate::utils::diff::adjust_hunk_line_numbers(
                        &local_patch,
                        context.line_offset.saturating_sub(1) as isize,
                    ),
                    first_line,
                    file_content: Some(context.content),
                };
            }
            Ok(_) => return diff_tool_inputs_only(file_path, &valid),
            Err(error) => {
                crate::utils::debug::log_for_debugging(&format!(
                    "Error reading edit context: {error}"
                ));
                return diff_tool_inputs_only(file_path, &valid);
            }
        }
    }

    match crate::utils::read_edit_context::read_capped(&mut file) {
        Ok(Some(file_content)) => {
            let normalized = valid
                .iter()
                .map(|edit| normalize_edit(&file_content, edit))
                .collect::<Vec<_>>();
            let first_line = file_content
                .split_once('\n')
                .map_or(file_content.as_str(), |(first, _)| first)
                .to_string();
            DiffData {
                patch: get_patch_for_display(&file_content, &display_edits(&normalized)),
                first_line: Some(first_line),
                file_content: Some(file_content),
            }
        }
        Ok(None) => diff_tool_inputs_only(file_path, &valid),
        Err(error) => {
            crate::utils::debug::log_for_debugging(&format!(
                "Error loading full file for edit diff: {error}"
            ));
            diff_tool_inputs_only(file_path, &valid)
        }
    }
}

/// Maps to: CC `components/FileEditToolDiff.tsx#diffToolInputsOnly`.
fn diff_tool_inputs_only(file_path: &str, edits: &[FileEdit]) -> DiffData {
    let _ = file_path; // upstream passes filePath into getPatchForDisplay for labeling
    DiffData {
        patch: edits
            .iter()
            .flat_map(|edit| {
                get_patch_for_display(
                    &edit.old_string,
                    &[DisplayEdit {
                        old_string: &edit.old_string,
                        new_string: &edit.new_string,
                        replace_all: edit.replace_all,
                    }],
                )
            })
            .collect(),
        first_line: None,
        file_content: None,
    }
}

/// Maps to: CC `components/FileEditToolDiff.tsx#normalizeEdit`.
fn normalize_edit(file_content: &str, edit: &FileEdit) -> FileEdit {
    let actual_old = find_actual_string(file_content, &edit.old_string)
        .unwrap_or_else(|| edit.old_string.clone());
    let actual_new = preserve_quote_style(&edit.old_string, &actual_old, &edit.new_string);
    FileEdit {
        old_string: actual_old,
        new_string: actual_new,
        replace_all: edit.replace_all,
    }
}

fn display_edits(edits: &[FileEdit]) -> Vec<DisplayEdit<'_>> {
    edits
        .iter()
        .map(|edit| DisplayEdit {
            old_string: &edit.old_string,
            new_string: &edit.new_string,
            replace_all: edit.replace_all,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    fn temp_file(name: &str, content: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("nyan-file-edit-diff-{name}-{}", std::process::id()));
        std::fs::write(&path, content).unwrap();
        path
    }

    fn patch_lines(data: &DiffData) -> Vec<&str> {
        data.patch
            .iter()
            .flat_map(|hunk| hunk.lines.iter().map(String::as_str))
            .collect()
    }

    #[test]
    fn load_diff_data_uses_file_snapshot_context_when_available() {
        let path = temp_file("context", "alpha\nold\nomega\n");
        let data = load_diff_data(
            &path.display().to_string(),
            &[FileEdit {
                old_string: "old".to_string(),
                new_string: "new".to_string(),
                replace_all: false,
            }],
        );
        let _ = std::fs::remove_file(path);
        let lines = patch_lines(&data);

        assert!(lines.contains(&" alpha"), "lines={lines:?}");
        assert!(lines.contains(&"-old"), "lines={lines:?}");
        assert!(lines.contains(&"+new"), "lines={lines:?}");
        assert!(lines.contains(&" omega"), "lines={lines:?}");
        assert_eq!(data.first_line.as_deref(), Some("alpha"));
        assert!(data.file_content.is_some());
    }

    #[test]
    fn load_diff_data_adjusts_context_hunk_numbers_to_original_file() {
        let content = (1..=20)
            .map(|line| {
                if line == 15 {
                    "old".to_string()
                } else {
                    format!("line-{line}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let path = temp_file("offset", &content);
        let data = load_diff_data(
            &path.display().to_string(),
            &[FileEdit {
                old_string: "old".to_string(),
                new_string: "new".to_string(),
                replace_all: false,
            }],
        );
        let _ = std::fs::remove_file(path);
        assert!(data.patch.iter().any(|hunk| hunk.old_start > 1));
        assert_eq!(data.first_line, None);
        assert!(patch_lines(&data).contains(&"-old"));
    }

    #[test]
    fn load_diff_data_falls_back_to_tool_inputs_when_file_missing() {
        let path = std::env::temp_dir().join(format!(
            "nyan-file-edit-diff-missing-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let data = load_diff_data(
            &path.display().to_string(),
            &[FileEdit {
                old_string: "old".to_string(),
                new_string: "new".to_string(),
                replace_all: false,
            }],
        );
        let lines = patch_lines(&data);
        assert!(lines.contains(&"-old"), "lines={lines:?}");
        assert!(lines.contains(&"+new"), "lines={lines:?}");
        assert!(data.file_content.is_none());
    }

    #[test]
    fn load_diff_data_uses_tool_inputs_when_needle_is_absent_from_existing_file() {
        let path = temp_file("needle-absent", "different content\n");
        let data = load_diff_data(
            &path.display().to_string(),
            &[FileEdit {
                old_string: "old".to_string(),
                new_string: "new".to_string(),
                replace_all: false,
            }],
        );
        let _ = std::fs::remove_file(path);
        let lines = patch_lines(&data);
        assert!(lines.contains(&"-old"));
        assert!(lines.contains(&"+new"));
        assert!(data.file_content.is_none());
    }

    #[test]
    fn load_diff_data_falls_back_to_model_quotes_when_chunk_scan_cannot_match_curly_file() {
        let path = temp_file("quotes", "let value = “old”;\n");
        let data = load_diff_data(
            &path.display().to_string(),
            &[FileEdit {
                old_string: "let value = \"old\";".to_string(),
                new_string: "let value = \"new\";".to_string(),
                replace_all: false,
            }],
        );
        let _ = std::fs::remove_file(path);
        let lines = patch_lines(&data);

        assert!(lines.contains(&"-let value = \"old\";"), "lines={lines:?}");
        assert!(lines.contains(&"+let value = \"new\";"), "lines={lines:?}");
    }

    #[test]
    fn find_actual_string_uses_normalized_char_offset_for_curly_prefixes() {
        let actual = find_actual_string("prefix “quoted” then target", "then target");
        assert_eq!(actual.as_deref(), Some("then target"));
    }

    #[test]
    fn file_edit_tool_diff_component_renders_official_pending_placeholder() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                FileEditToolDiff(
                    file_path: "/missing/demo.txt".to_string(),
                    edits: vec![FileEdit {
                        old_string: "old".to_string(),
                        new_string: "new".to_string(),
                        replace_all: false,
                    }],
                )
            }
        }
        .render(Some(80))
        .to_string();
        assert!(text.contains('…'), "canvas=\n{text}");
    }
}
