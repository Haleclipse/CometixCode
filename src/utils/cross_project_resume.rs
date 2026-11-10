//! Maps to: CC `utils/crossProjectResume.ts`.

use crate::utils::bash::shell_quote::quote;
use crate::utils::session_storage::SessionSelection;

/// Rust enum projection of CC `CrossProjectResumeResult`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CrossProjectResumeResult {
    SameProject,
    DifferentProject {
        project_path: String,
        command: String,
    },
}

/// Maps to: CC `utils/crossProjectResume.ts#checkCrossProjectResume`.
///
/// CC's same-repo worktree shortcut is a staged rollout that external builds
/// never take, so even same-repo worktrees print a `cd ... && nyan --resume ...`.
pub fn check_cross_project_resume(
    selection: &SessionSelection,
    show_all_projects: bool,
) -> CrossProjectResumeResult {
    let current_project_path = crate::bootstrap::state::get_original_cwd()
        .display()
        .to_string();
    let Some(project_path) = selection
        .project_path
        .as_deref()
        .filter(|path| !path.is_empty())
    else {
        return CrossProjectResumeResult::SameProject;
    };

    if !show_all_projects || project_path == current_project_path {
        return CrossProjectResumeResult::SameProject;
    }

    CrossProjectResumeResult::DifferentProject {
        project_path: project_path.to_string(),
        command: format!(
            "cd {} && nyan --resume {}",
            quote(&[project_path]),
            selection.session_id
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn check_cross_project_resume_matches_official_worktree_gate_and_command() {
        let previous_original_cwd = crate::bootstrap::state::get_original_cwd();
        crate::bootstrap::state::set_original_cwd("/repo");
        let worktree_selection = SessionSelection {
            session_id: "abc".to_string(),
            project_path: Some("/repo-wt/subdir".to_string()),
            file_path: PathBuf::from("/repo-wt/subdir/abc.jsonl"),
        };

        assert_eq!(
            check_cross_project_resume(&worktree_selection, true),
            CrossProjectResumeResult::DifferentProject {
                project_path: "/repo-wt/subdir".to_string(),
                command: "cd /repo-wt/subdir && nyan --resume abc".to_string(),
            }
        );

        let other_selection = SessionSelection {
            session_id: "abc".to_string(),
            project_path: Some("/other project".to_string()),
            file_path: PathBuf::from("/other project/abc.jsonl"),
        };
        assert_eq!(
            check_cross_project_resume(&other_selection, true),
            CrossProjectResumeResult::DifferentProject {
                project_path: "/other project".to_string(),
                command: "cd '/other project' && nyan --resume abc".to_string(),
            }
        );

        crate::bootstrap::state::set_original_cwd(previous_original_cwd);
    }
}
