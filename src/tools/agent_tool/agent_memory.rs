//! Persistent agent memory helpers.
//! Maps to CC `tools/AgentTool/agentMemory.ts`.
//!
//! Safety boundary: the official TypeScript helper fire-and-forgets directory
//! creation from `loadAgentMemoryPrompt(...)`. Cometix keeps this prompt helper
//! read-only: it computes the same memory directory and reads `MEMORY.md` via
//! `memdir::build_memory_prompt`, but does not create directories or write
//! memory files in this slice.

use std::path::{Path, PathBuf};

use crate::memdir::paths::get_memory_base_dir;
use crate::utils::process_env::JsTruthy;

/// Maps to CC `agentMemory.ts#AgentMemoryScope`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AgentMemoryScope {
    User,
    Project,
    Local,
}

impl AgentMemoryScope {
    pub fn official_name(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
            Self::Local => "local",
        }
    }
}

/// Maps to CC `agentMemory.ts#sanitizeAgentTypeForPath`.
pub fn sanitize_agent_type_for_path(agent_type: &str) -> String {
    agent_type.replace(':', "-")
}

/// Maps to CC `agentMemory.ts#getAgentMemoryDir`.
pub fn get_agent_memory_dir(agent_type: &str, scope: AgentMemoryScope, cwd: &Path) -> PathBuf {
    let dir_name = sanitize_agent_type_for_path(agent_type);
    match scope {
        AgentMemoryScope::Project => cwd.join(".claude").join("agent-memory").join(dir_name),
        AgentMemoryScope::Local => get_local_agent_memory_dir(&dir_name, cwd),
        AgentMemoryScope::User => get_memory_base_dir().join("agent-memory").join(dir_name),
    }
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn is_strictly_inside(path: &Path, directory: &Path) -> bool {
    path != directory && path.starts_with(directory)
}

/// Maps to CC `agentMemory.ts#isAgentMemoryPath`.
pub fn is_agent_memory_path(absolute_path: &Path, cwd: &Path) -> bool {
    let normalized = normalize_lexically(absolute_path);
    let memory_base = get_memory_base_dir();
    if is_strictly_inside(&normalized, &memory_base.join("agent-memory"))
        || is_strictly_inside(&normalized, &cwd.join(".claude").join("agent-memory"))
    {
        return true;
    }
    if let Some(remote_memory_dir) =
        crate::utils::process_env::var("CLAUDE_CODE_REMOTE_MEMORY_DIR").truthy()
    {
        let remote_projects = PathBuf::from(remote_memory_dir).join("projects");
        normalized.starts_with(&remote_projects)
            && normalized.components().any(|component| {
                component.as_os_str() == std::ffi::OsStr::new("agent-memory-local")
            })
    } else {
        is_strictly_inside(&normalized, &cwd.join(".claude").join("agent-memory-local"))
    }
}

/// Maps to CC `agentMemory.ts#getAgentMemoryEntrypoint`.
pub fn get_agent_memory_entrypoint(
    agent_type: &str,
    scope: AgentMemoryScope,
    cwd: &Path,
) -> PathBuf {
    get_agent_memory_dir(agent_type, scope, cwd).join("MEMORY.md")
}

/// Maps to CC `agentMemory.ts#getMemoryScopeDisplay`.
pub fn get_memory_scope_display(memory: Option<AgentMemoryScope>, cwd: &Path) -> String {
    match memory {
        Some(AgentMemoryScope::User) => {
            format!("User ({}/agent-memory/)", get_memory_base_dir().display())
        }
        Some(AgentMemoryScope::Project) => "Project (.claude/agent-memory/)".to_string(),
        Some(AgentMemoryScope::Local) => {
            format!(
                "Local ({}/)",
                get_local_agent_memory_dir("...", cwd).display()
            )
        }
        None => "None".to_string(),
    }
}

/// Maps to CC `agentMemory.ts#loadAgentMemoryPrompt`.
pub fn load_agent_memory_prompt(agent_type: &str, scope: AgentMemoryScope, cwd: &Path) -> String {
    let scope_note = match scope {
        AgentMemoryScope::User => {
            "- Since this memory is user-scope, keep learnings general since they apply across all projects"
        }
        AgentMemoryScope::Project => {
            "- Since this memory is project-scope and shared with your team via version control, tailor your memories to this project"
        }
        AgentMemoryScope::Local => {
            "- Since this memory is local-scope (not checked into version control), tailor your memories to this project and machine"
        }
    };

    let memory_dir = get_agent_memory_dir(agent_type, scope, cwd);
    let mut extra_guidelines = vec![scope_note.to_string()];
    // CC `agentMemory.ts:167-175`: `x && x.trim().length > 0 ? [x]` — tested
    // trimmed, passed as is (as `memdir.rs` does).
    if let Some(extra) = crate::utils::process_env::var("CLAUDE_COWORK_MEMORY_EXTRA_GUIDELINES")
        .filter(|value| !value.trim().is_empty())
    {
        extra_guidelines.push(extra);
    }

    crate::memdir::memdir::build_memory_prompt(
        "Persistent Agent Memory",
        &memory_dir,
        Some(&extra_guidelines),
    )
}

fn get_local_agent_memory_dir(dir_name: &str, cwd: &Path) -> PathBuf {
    if let Some(remote_memory_dir) =
        crate::utils::process_env::var("CLAUDE_CODE_REMOTE_MEMORY_DIR").truthy()
    {
        // CC agentMemory.ts:35-36:
        // `sanitizePath(findCanonicalGitRoot(getProjectRoot()) ?? getProjectRoot())`
        // — the canonical-worktree git root (#24382) run through the shared
        // sanitizePath (200-char cap + hash suffix), not a bespoke sanitizer.
        // Deviation: CC anchors on `getProjectRoot()` (STATE.projectRoot, never
        // updated mid-session); this port has no projectRoot slot yet, so
        // `get_original_cwd()` is the nearest anchor — unlike CC's, it IS
        // updated by the worktree tools (exit_worktree_tool), so the remote
        // namespace can drift across worktree enter/exit.
        let project_root = crate::bootstrap::state::get_original_cwd();
        let canonical =
            crate::utils::git::find_canonical_git_root(&project_root).unwrap_or(project_root);
        return PathBuf::from(remote_memory_dir)
            .join("projects")
            .join(crate::utils::session_storage::sanitize_path(
                &canonical.display().to_string(),
            ))
            .join("agent-memory-local")
            .join(dir_name);
    }
    cwd.join(".claude")
        .join("agent-memory-local")
        .join(dir_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn agent_memory_paths_match_official_scopes_and_colon_sanitizing() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _remote_memory = EnvVarGuard::unset("CLAUDE_CODE_REMOTE_MEMORY_DIR");
        let root = std::env::temp_dir().join(format!(
            "cometix-agent-memory-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let cwd = root.as_path();

        assert_eq!(
            sanitize_agent_type_for_path("plugin:reviewer"),
            "plugin-reviewer"
        );
        assert!(
            get_agent_memory_dir("plugin:reviewer", AgentMemoryScope::Project, cwd)
                .ends_with(".claude/agent-memory/plugin-reviewer")
        );
        assert!(
            get_agent_memory_dir("plugin:reviewer", AgentMemoryScope::Local, cwd)
                .ends_with(".claude/agent-memory-local/plugin-reviewer")
        );
        assert!(is_agent_memory_path(
            &cwd.join(".claude/agent-memory/reviewer/MEMORY.md"),
            cwd,
        ));
        assert!(is_agent_memory_path(
            &cwd.join(".claude/agent-memory-local/reviewer/MEMORY.md"),
            cwd,
        ));

        crate::utils::process_env::set("CLAUDE_CODE_REMOTE_MEMORY_DIR", root.join("remote"));
        let local = get_agent_memory_dir("plugin:reviewer", AgentMemoryScope::Local, cwd);
        assert!(
            local
                .to_string_lossy()
                .contains("agent-memory-local/plugin-reviewer")
        );
        assert!(local.to_string_lossy().contains("projects"));
        crate::utils::process_env::remove("CLAUDE_CODE_REMOTE_MEMORY_DIR");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn load_agent_memory_prompt_reads_entrypoint_without_creating_dirs() {
        let root = std::env::temp_dir().join(format!(
            "cometix-agent-memory-prompt-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let memory_dir = root.join(".claude/agent-memory/reviewer");
        std::fs::create_dir_all(&memory_dir).unwrap();
        std::fs::write(memory_dir.join("MEMORY.md"), "- Prefer focused reviews\n").unwrap();

        let prompt = load_agent_memory_prompt("reviewer", AgentMemoryScope::Project, &root);
        assert!(prompt.contains("Persistent Agent Memory"));
        assert!(prompt.contains("Since this memory is project-scope"));
        assert!(prompt.contains("- Prefer focused reviews"));
        let _ = std::fs::remove_dir_all(root);
    }
}
