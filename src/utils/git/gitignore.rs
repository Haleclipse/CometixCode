//! Gitignore helpers.
//!
//! Maps to: CC `utils/git/gitignore.ts`.

use std::path::Path;
use std::time::Duration;

/// Maps to: CC `utils/git/gitignore.ts#isPathGitignored`.
pub fn is_path_gitignored(file_path: &Path, cwd: &Path) -> bool {
    let file_path = file_path.to_string_lossy();
    crate::utils::exec_file_no_throw::exec_file_no_throw_with_cwd(
        "git",
        &["check-ignore", file_path.as_ref()],
        Duration::from_secs(10 * 60),
        Some(cwd),
        false,
    )
    .code
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_path_gitignored_uses_git_precedence_and_fails_open_outside_repo() {
        let root = std::env::temp_dir().join(format!(
            "cometix-gitignore-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(root.join("ignored/nested")).unwrap();
        std::fs::write(root.join(".gitignore"), "ignored/\n").unwrap();
        let _ = crate::utils::subprocess_env::command("git")
            .args(["init", "--quiet"])
            .current_dir(&root)
            .status();

        assert!(is_path_gitignored(&root.join("ignored"), &root));
        assert!(!is_path_gitignored(&root.join("visible"), &root));

        let outside = root.join("not-a-repo");
        std::fs::create_dir_all(&outside).unwrap();
        assert!(!is_path_gitignored(&outside.join("anything"), &outside));
        let _ = std::fs::remove_dir_all(root);
    }
}
