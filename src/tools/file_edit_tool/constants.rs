//! Maps to: CC `tools/FileEditTool/constants.ts`.

/// Maps to: CC `FILE_EDIT_TOOL_NAME`.
pub const FILE_EDIT_TOOL_NAME: &str = "Edit";

/// Permission pattern for granting session-level access to the project's
/// `.nyan/` folder. Maps to: CC `CLAUDE_FOLDER_PERMISSION_PATTERN`.
pub const CLAUDE_FOLDER_PERMISSION_PATTERN: &str = "/.nyan/**";

/// Permission pattern for granting session-level access to the global
/// `~/.nyan/` folder. Maps to: CC `GLOBAL_CLAUDE_FOLDER_PERMISSION_PATTERN`.
pub const GLOBAL_CLAUDE_FOLDER_PERMISSION_PATTERN: &str = "~/.nyan/**";

/// Maps to: CC `FILE_UNEXPECTEDLY_MODIFIED_ERROR`.
pub const FILE_UNEXPECTEDLY_MODIFIED_ERROR: &str =
    "File has been unexpectedly modified. Read it again before attempting to write it.";

/// V8/Bun string length guard — 1 GiB stat bytes. Maps to: CC
/// `FileEditTool.ts` `MAX_EDIT_FILE_SIZE`.
pub const MAX_EDIT_FILE_SIZE: u64 = 1024 * 1024 * 1024;
