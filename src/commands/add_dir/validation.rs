//! Maps to CC `commands/add-dir/validation.ts`.

use crate::tool::ToolPermissionContext;
use crate::utils::errors::get_errno_code;
use crate::utils::path::expand_path;
use crate::utils::permissions::filesystem::{all_working_directories, path_in_working_path};
use serde::{Deserialize, Serialize};

/// Maps to CC `commands/add-dir/validation.ts:13-29` `AddDirectoryResult`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "resultType",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AddDirectoryResult {
    Success {
        absolute_path: String,
    },
    EmptyPath,
    PathNotFound {
        directory_path: String,
        absolute_path: String,
    },
    NotADirectory {
        directory_path: String,
        absolute_path: String,
    },
    AlreadyInWorkingDirectory {
        directory_path: String,
        working_dir: String,
    },
}

/// Maps to CC `commands/add-dir/validation.ts:31-92`.
pub async fn validate_directory_for_workspace(
    directory_path: &str,
    permission_context: &ToolPermissionContext,
) -> anyhow::Result<AddDirectoryResult> {
    if directory_path.is_empty() {
        return Ok(AddDirectoryResult::EmptyPath);
    }

    // CC :41-43 resolve(expandPath(...)); PathBuf components strip trailing
    // separators without resolving symlinks, preserving the lexical storage key.
    let expanded = expand_path(directory_path, None).map_err(anyhow::Error::msg)?;
    let absolute_path = expanded
        .components()
        .collect::<std::path::PathBuf>()
        .to_string_lossy()
        .into_owned();
    match tokio::fs::metadata(&absolute_path).await {
        Ok(stats) if !stats.is_dir() => {
            return Ok(AddDirectoryResult::NotADirectory {
                directory_path: directory_path.to_string(),
                absolute_path,
            });
        }
        Ok(_) => {}
        Err(error) => {
            let error = anyhow::Error::new(error);
            if matches!(
                get_errno_code(&error),
                Some("ENOENT" | "ENOTDIR" | "EACCES" | "EPERM")
            ) {
                return Ok(AddDirectoryResult::PathNotFound {
                    directory_path: directory_path.to_string(),
                    absolute_path,
                });
            }
            return Err(error);
        }
    }

    for working_dir in all_working_directories(permission_context) {
        if path_in_working_path(&absolute_path, &working_dir) {
            return Ok(AddDirectoryResult::AlreadyInWorkingDirectory {
                directory_path: directory_path.to_string(),
                working_dir,
            });
        }
    }
    Ok(AddDirectoryResult::Success { absolute_path })
}

/// Maps to CC `commands/add-dir/validation.ts:95-111` `addDirHelpMessage`.
/// Exact text for Chalk's no-color output. Partial seam: the shared Chalk color
/// detection/styling owner is not ported, so TTY/forced-color bold is not applied.
pub fn add_dir_help_message(result: &AddDirectoryResult) -> String {
    match result {
        AddDirectoryResult::EmptyPath => "Please provide a directory path.".to_string(),
        AddDirectoryResult::PathNotFound { absolute_path, .. } => {
            format!("Path {absolute_path} was not found.")
        }
        AddDirectoryResult::NotADirectory {
            directory_path,
            absolute_path,
        } => {
            let parent_dir = std::path::Path::new(absolute_path)
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .to_string_lossy();
            format!(
                "{directory_path} is not a directory. Did you mean to add the parent directory {parent_dir}?"
            )
        }
        AddDirectoryResult::AlreadyInWorkingDirectory {
            directory_path,
            working_dir,
        } => {
            format!(
                "{directory_path} is already accessible within the existing working directory {working_dir}."
            )
        }
        AddDirectoryResult::Success { absolute_path } => {
            format!("Added {absolute_path} as a working directory.")
        }
    }
}

#[cfg(test)]
mod tests {
    //! Official behavior oracles for CC `commands/add-dir/validation.ts`.

    use super::*;
    use crate::types::permissions::{AdditionalWorkingDirectory, PermissionRuleSource};
    use crate::utils::test_env::TEST_ENV_LOCK;
    use std::path::PathBuf;

    struct Workspace {
        root: PathBuf,
        original_cwd: PathBuf,
    }

    impl Workspace {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("cc-add-dir-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(root.join("original")).unwrap();
            let original_cwd = crate::bootstrap::state::get_original_cwd();
            crate::bootstrap::state::set_original_cwd(root.join("original"));
            Self { root, original_cwd }
        }

        fn path(&self, suffix: &str) -> String {
            self.root.join(suffix).to_string_lossy().into_owned()
        }
    }

    impl Drop for Workspace {
        fn drop(&mut self) {
            crate::bootstrap::state::set_original_cwd(self.original_cwd.clone());
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[tokio::test]
    async fn validation_matches_official_result_variants_and_stat_before_containment() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let workspace = Workspace::new();
        let context = ToolPermissionContext::default();
        let directory = workspace.path("additional");
        std::fs::create_dir(&directory).unwrap();
        // CC validation.ts:35-39 tests only the original input's emptiness.
        assert_eq!(
            validate_directory_for_workspace("", &context)
                .await
                .unwrap(),
            AddDirectoryResult::EmptyPath
        );
        // CC :45-71 checks stat before containment, including missing/file inside cwd.
        let file = workspace.path("original/file");
        std::fs::write(&file, "file").unwrap();
        for path in [workspace.path("original/missing"), format!("{file}/child")] {
            assert_eq!(
                validate_directory_for_workspace(&path, &context)
                    .await
                    .unwrap(),
                AddDirectoryResult::PathNotFound {
                    directory_path: path.clone(),
                    absolute_path: path,
                }
            );
        }
        assert_eq!(
            validate_directory_for_workspace(&file, &context)
                .await
                .unwrap(),
            AddDirectoryResult::NotADirectory {
                directory_path: file.clone(),
                absolute_path: file,
            }
        );
        let original = workspace.path("original");
        assert_eq!(
            validate_directory_for_workspace(&original, &context)
                .await
                .unwrap(),
            AddDirectoryResult::AlreadyInWorkingDirectory {
                directory_path: original.clone(),
                working_dir: original,
            }
        );
        assert_eq!(
            validate_directory_for_workspace(&directory, &context)
                .await
                .unwrap(),
            AddDirectoryResult::Success {
                absolute_path: directory
            }
        );
    }

    #[tokio::test]
    async fn validation_matches_official_normalization_and_literal_whitespace() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let workspace = Workspace::new();
        let context = ToolPermissionContext::default();
        let directory = workspace.path("caf\u{e9}");
        std::fs::create_dir(&directory).unwrap();
        // CC validation.ts:43 resolve(expandPath(...)); path.ts:54-81 JS trim + NFC.
        for input in [
            format!("{directory}/"),
            format!(" \u{feff}{directory}\u{feff} "),
            workspace.path("cafe\u{301}"),
        ] {
            assert_eq!(
                validate_directory_for_workspace(&input, &context)
                    .await
                    .unwrap(),
                AddDirectoryResult::Success {
                    absolute_path: directory.clone()
                }
            );
        }
        let literal = workspace.path("\u{85}");
        std::fs::create_dir(&literal).unwrap();
        assert_eq!(
            validate_directory_for_workspace(&literal, &context)
                .await
                .unwrap(),
            AddDirectoryResult::Success {
                absolute_path: literal
            }
        );
        // Whitespace input resolves to cwd; it does not return emptyPath.
        for input in [" \t\r\n", "\u{feff}"] {
            assert!(!matches!(
                validate_directory_for_workspace(input, &context)
                    .await
                    .unwrap(),
                AddDirectoryResult::EmptyPath
            ));
        }
        assert_eq!(
            validate_directory_for_workspace("bad\0path", &context)
                .await
                .unwrap_err()
                .to_string(),
            "Path contains null bytes"
        );
    }

    #[tokio::test]
    async fn validation_matches_official_first_containing_directory_in_insertion_order() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let workspace = Workspace::new();
        let mut context = ToolPermissionContext::default();
        let parent = workspace.path("parent");
        let child = workspace.path("parent/child");
        std::fs::create_dir_all(&child).unwrap();
        for directory in [&child, &parent] {
            context.additional_working_directories.insert(
                directory.clone(),
                AdditionalWorkingDirectory {
                    path: directory.clone(),
                    source: PermissionRuleSource::CliArg,
                },
            );
        }
        // CC validation.ts:76-86 preserves allWorkingDirectories Set order.
        assert_eq!(
            validate_directory_for_workspace(&child, &context)
                .await
                .unwrap(),
            AddDirectoryResult::AlreadyInWorkingDirectory {
                directory_path: child.clone(),
                working_dir: child,
            }
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn validation_matches_official_symlink_spelling_and_fatal_eloop() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let workspace = Workspace::new();
        let context = ToolPermissionContext::default();
        let link = workspace.path("link");
        std::os::unix::fs::symlink(workspace.path("original"), &link).unwrap();
        // CC :43 stat follows symlinks, but the stored success key stays lexical.
        assert_eq!(
            validate_directory_for_workspace(&link, &context)
                .await
                .unwrap(),
            AddDirectoryResult::Success {
                absolute_path: link
            }
        );
        let looping = workspace.path("loop");
        std::os::unix::fs::symlink(&looping, &looping).unwrap();
        // CC :58-72 only four errno values are silent; ELOOP is rethrown intact.
        let error = validate_directory_for_workspace(&looping, &context)
            .await
            .unwrap_err();
        assert_eq!(get_errno_code(&error), Some("ELOOP"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn validation_matches_official_inaccessible_path_is_silent_not_found() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let workspace = Workspace::new();
        let private = workspace.root.join("private");
        std::fs::create_dir_all(private.join("child")).unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o000)).unwrap();
        let path = workspace.path("private/child");
        let result =
            validate_directory_for_workspace(&path, &ToolPermissionContext::default()).await;
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
        // CC :58-69 treats EACCES/EPERM as pathNotFound. Root bypasses DAC.
        if unsafe { libc::geteuid() } != 0 {
            assert_eq!(
                result.unwrap(),
                AddDirectoryResult::PathNotFound {
                    directory_path: path.clone(),
                    absolute_path: path
                }
            );
        }
    }

    #[test]
    fn help_message_matches_official_no_color_text_and_tagged_results() {
        use serde_json::json;
        // CC validation.ts:13-29 and :95-111: discriminants/field names and all
        // exact strings under Chalk's color-disabled mode.
        let cases = [
            (
                json!({"resultType":"emptyPath"}),
                "Please provide a directory path.",
            ),
            (
                json!({"resultType":"pathNotFound","directoryPath":"./missing","absolutePath":"/repo/missing"}),
                "Path /repo/missing was not found.",
            ),
            (
                json!({"resultType":"notADirectory","directoryPath":"./file","absolutePath":"/repo/file"}),
                "./file is not a directory. Did you mean to add the parent directory /repo?",
            ),
            (
                json!({"resultType":"alreadyInWorkingDirectory","directoryPath":"./src","workingDir":"/repo"}),
                "./src is already accessible within the existing working directory /repo.",
            ),
            (
                json!({"resultType":"success","absolutePath":"/extra"}),
                "Added /extra as a working directory.",
            ),
        ];
        for (value, expected) in cases {
            let result: AddDirectoryResult = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(add_dir_help_message(&result), expected);
            assert_eq!(serde_json::to_value(result).unwrap(), value);
        }
    }
}
