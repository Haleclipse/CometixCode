//! Maps to: CC `utils/embeddedTools.ts`.
//!
//! Whether this build has bfs/ugrep embedded in the binary (ant-native only).
//! When true: `find`/`grep` in Claude's Bash shell are shadowed by shell
//! functions invoking the binary with argv0='bfs'/'ugrep', the dedicated
//! Glob/Grep tools are removed, and prompt guidance steering away from
//! find/grep is omitted. Gated by the `EMBEDDED_SEARCH_TOOLS` build-time env.

/// Maps to: CC `hasEmbeddedSearchTools()`.
pub fn has_embedded_search_tools() -> bool {
    if !crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("EMBEDDED_SEARCH_TOOLS").as_deref(),
    ) {
        return false;
    }
    match crate::utils::process_env::var("CLAUDE_CODE_ENTRYPOINT").as_deref() {
        Some("sdk-ts") | Some("sdk-py") | Some("sdk-cli") | Some("local-agent") => false,
        _ => true,
    }
}

/// Maps to: CC `embeddedSearchToolsBinaryPath()`. Path to the binary containing
/// the embedded search tools; only meaningful when `has_embedded_search_tools()`.
pub fn embedded_search_tools_binary_path() -> std::path::PathBuf {
    std::env::current_exe().unwrap_or_default()
}
