//! Maps to CC `tools/FileEditTool/prompt.ts`.

use crate::tools::file_read_tool::prompt::FILE_READ_TOOL_NAME;
use crate::utils::file::is_compact_line_prefix_enabled;

fn get_pre_read_instruction() -> String {
    format!(
        "\n- You must use your `{FILE_READ_TOOL_NAME}` tool at least once in the conversation before editing. This tool will error if you attempt an edit without reading the file. "
    )
}

pub fn get_edit_tool_description() -> String {
    get_default_edit_description()
}

fn get_default_edit_description() -> String {
    let prefix_format = if is_compact_line_prefix_enabled() {
        "line number + tab"
    } else {
        "spaces + line number + arrow"
    };
    format!(
        "Performs exact string replacements in files.\n\nUsage:{}\n- When editing text from Read tool output, ensure you preserve the exact indentation (tabs/spaces) as it appears AFTER the line number prefix. The line number prefix format is: {prefix_format}. Everything after that is the actual file content to match. Never include any part of the line number prefix in the old_string or new_string.\n- ALWAYS prefer editing existing files in the codebase. NEVER write new files unless explicitly required.\n- Only use emojis if the user explicitly requests it. Avoid adding emojis to files unless asked.\n- The edit will FAIL if `old_string` is not unique in the file. Either provide a larger string with more surrounding context to make it unique or use `replace_all` to change every instance of `old_string`.\n- Use `replace_all` for replacing and renaming strings across the file. This parameter is useful if you want to rename a variable for instance.",
        get_pre_read_instruction()
    )
}
