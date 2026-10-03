//! Team-memory predicates used by Read/Search collapse.
//!
//! Maps to: CC `utils/teamMemoryOps.ts`. The module is exposed only in the
//! internal build, matching the source `feature('TEAMMEM')` boundary.

pub use crate::memdir::team_mem_paths::is_team_mem_file;

/// Maps to: CC `utils/teamMemoryOps.ts:10-22` `isTeamMemorySearch`.
pub fn is_team_memory_search(tool_input: Option<&serde_json::Value>) -> bool {
    tool_input
        .and_then(serde_json::Value::as_object)
        .and_then(|input| input.get("path"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|path| is_team_mem_file(std::path::Path::new(path)))
}

/// Maps to: CC `utils/teamMemoryOps.ts:26-38` `isTeamMemoryWriteOrEdit`.
pub fn is_team_memory_write_or_edit(
    tool_name: &str,
    tool_input: Option<&serde_json::Value>,
) -> bool {
    if tool_name != crate::tools::file_write_tool::prompt::FILE_WRITE_TOOL_NAME
        && tool_name != crate::tools::file_edit_tool::FILE_EDIT_TOOL_NAME
    {
        return false;
    }
    let Some(input) = tool_input.and_then(serde_json::Value::as_object) else {
        return false;
    };
    input
        .get("file_path")
        .or_else(|| input.get("path"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|path| is_team_mem_file(std::path::Path::new(path)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    struct GlobalConfigGuard(Option<crate::utils::config::GlobalConfig>);

    impl Drop for GlobalConfigGuard {
        fn drop(&mut self) {
            crate::utils::config::replace_test_global_config(self.0.take());
        }
    }

    #[test]
    fn team_memory_predicates_ignore_growthbook_delivery() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "cometix-team-memory-ops-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let _memory_env = [
            EnvVarGuard::set("CLAUDE_COWORK_MEMORY_PATH_OVERRIDE", &root),
            EnvVarGuard::set("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "false"),
        ];

        let mut config = crate::utils::config::GlobalConfig::default();
        config.cached_growth_book_features = Some(std::collections::HashMap::from([(
            "tengu_herring_clock".to_string(),
            serde_json::json!(true),
        )]));
        config.growth_book_overrides = Some(std::collections::HashMap::from([(
            "tengu_herring_clock".to_string(),
            serde_json::json!(true),
        )]));
        let _config = GlobalConfigGuard(crate::utils::config::replace_test_global_config(Some(
            config,
        )));

        let team_file = root.join("team/MEMORY.md").display().to_string();
        // Field extraction and path matching are unchanged; the cohort gate now
        // comes from the source-controlled switch table, so the injected cache
        // cannot turn team-memory collapse on.
        assert!(crate::memdir::team_mem_paths::is_team_mem_path(
            std::path::Path::new(&team_file)
        ));
        assert!(!crate::memdir::team_mem_paths::is_team_memory_enabled());
        assert!(!is_team_memory_search(Some(
            &serde_json::json!({"path": team_file.clone()})
        )));
        assert!(!is_team_memory_search(Some(
            &serde_json::json!({"glob": "team/*.md"})
        )));
        assert!(!is_team_memory_write_or_edit(
            "Write",
            Some(&serde_json::json!({"file_path": team_file.clone()}))
        ));
        assert!(!is_team_memory_write_or_edit(
            "MultiEdit",
            Some(&serde_json::json!({"file_path": team_file}))
        ));
    }
}

/// Maps to: CC `utils/teamMemoryOps.ts:42-88#appendTeamMemorySummaryParts`.
///
/// CC's sole caller is `collapseReadSearch.ts:1018`, reached only under
/// `feature('TEAMMEM')` (`:1017` `if (feature('TEAMMEM') && teamMemOps)`,
/// where `teamMemOps` is itself the TEAMMEM-conditional `require` at
/// `:34-36`, so the two conjuncts are one gate). The definition carries the
/// same cfg as that call site.
///
/// Part order and verb tense follow CC exactly: recall → search → write, with
/// the verb capitalised only when it opens the sentence (`parts.length === 0`)
/// and in present tense only while `isActive`.
///
/// The verb and plural ternaries are written out per branch because that is how
/// CC writes them — `:54-60`, `:68-74`, `:78-84` each inline the nested
/// conditional, and `:63`/`:85` inline `count === 1 ? 'memory' : 'memories'`.
/// They were briefly shared with `collapse_read_search.rs` as `summary_verb` /
/// `plural`, which reads like de-duplication but creates a cross-owner helper
/// with no upstream symbol behind it. Repetition is the faithful shape here.
#[cfg(feature = "anthropic_internal")]
pub fn append_team_memory_summary_parts(
    counts: &crate::utils::collapse_read_search::SearchReadMemoryCounts,
    is_active: bool,
    parts: &mut Vec<String>,
) {
    if counts.team_memory_read_count > 0 {
        let verb = if is_active {
            if parts.is_empty() {
                "Recalling"
            } else {
                "recalling"
            }
        } else if parts.is_empty() {
            "Recalled"
        } else {
            "recalled"
        };
        let count = counts.team_memory_read_count;
        let noun = if count == 1 { "memory" } else { "memories" };
        parts.push(format!("{verb} {count} team {noun}"));
    }
    if counts.team_memory_search_count > 0 {
        let verb = if is_active {
            if parts.is_empty() {
                "Searching"
            } else {
                "searching"
            }
        } else if parts.is_empty() {
            "Searched"
        } else {
            "searched"
        };
        parts.push(format!("{verb} team memories"));
    }
    if counts.team_memory_write_count > 0 {
        let verb = if is_active {
            if parts.is_empty() {
                "Writing"
            } else {
                "writing"
            }
        } else if parts.is_empty() {
            "Wrote"
        } else {
            "wrote"
        };
        let count = counts.team_memory_write_count;
        let noun = if count == 1 { "memory" } else { "memories" };
        parts.push(format!("{verb} {count} team {noun}"));
    }
}
