#[test]
fn duplicate_path_reuses_safe_resolution_and_remembers_missing_paths() {
    let root = std::env::temp_dir().join(format!("plugin-dedup-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("file");
    std::fs::write(&file, "").unwrap();
    let mut seen = std::collections::HashSet::new();
    assert!(!super::is_duplicate_path(
        crate::utils::fs_operations::get_fs_implementation().as_ref(),
        &file,
        &mut seen
    ));
    assert!(super::is_duplicate_path(
        crate::utils::fs_operations::get_fs_implementation().as_ref(),
        &file,
        &mut seen
    ));
    #[cfg(unix)]
    {
        let alias = root.join("alias");
        std::os::unix::fs::symlink(&file, &alias).unwrap();
        assert!(super::is_duplicate_path(
            crate::utils::fs_operations::get_fs_implementation().as_ref(),
            &alias,
            &mut seen
        ));
    }
    let missing = root.join("missing");
    assert!(!super::is_duplicate_path(
        crate::utils::fs_operations::get_fs_implementation().as_ref(),
        &missing,
        &mut seen
    ));
    assert!(super::is_duplicate_path(
        crate::utils::fs_operations::get_fs_implementation().as_ref(),
        &missing,
        &mut seen
    ));
    // Source Set<string> preserves spelling when safeResolvePath falls back.
    let trailing = std::path::PathBuf::from(format!("{}/", missing.display()));
    let dot = std::path::PathBuf::from(format!("{}/.", missing.display()));
    assert!(!super::is_duplicate_path(
        crate::utils::fs_operations::get_fs_implementation().as_ref(),
        &trailing,
        &mut seen
    ));
    assert!(!super::is_duplicate_path(
        crate::utils::fs_operations::get_fs_implementation().as_ref(),
        &dot,
        &mut seen
    ));
    assert!(super::is_duplicate_path(
        crate::utils::fs_operations::get_fs_implementation().as_ref(),
        &trailing,
        &mut seen
    ));
    std::fs::remove_dir_all(root).unwrap();
}

use super::*;

#[tokio::test]
async fn mkdir_matches_official_recursive_mode_and_eexist_catch() {
    let root = std::env::temp_dir().join(format!("mkdir-parity-{}", uuid::Uuid::new_v4()));
    let nested = root.join("nested/leaf");
    mkdir(&nested, Some(0o700)).await.unwrap();
    assert!(nested.is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&nested).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    mkdir(&nested, None).await.unwrap();
    let file = root.join("ordinary-file");
    std::fs::write(&file, "unchanged").unwrap();
    // CC swallows EEXIST even when the target is a plain file on macOS.
    mkdir(&file, None).await.unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "unchanged");
    // A non-directory ancestor yields ENOTDIR, which is not swallowed.
    let error = mkdir(&file.join("child"), None).await.unwrap_err();
    assert_eq!(crate::utils::errors::io_errno_code(&error), Some("ENOTDIR"));
    std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn readdir_matches_official_node_filename_order_and_entry_types() {
    // fsOperations.ts:398-400 delegates to Node withFileTypes. The local
    // Node oracle includes mixed case and non-ASCII names, not locale sort.
    let root = std::env::temp_dir().join(format!("readdir-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    for name in ["z-dir", "é-dir", "a-lower", "A-upper"] {
        std::fs::create_dir(root.join(name)).unwrap();
    }
    std::fs::write(root.join("file"), "").unwrap();
    let entries = readdir(&root).await.unwrap();
    let names: Vec<_> = entries.iter().map(|entry| entry.file_name()).collect();
    assert_eq!(names, ["A-upper", "a-lower", "file", "z-dir", "é-dir"]);
    assert!(entries[0].file_type().unwrap().is_dir());
    assert!(entries[2].file_type().unwrap().is_file());
    std::fs::remove_dir_all(&root).unwrap();
    assert_eq!(
        readdir(&root).await.unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
}

#[test]
#[cfg(unix)]
fn safe_resolve_path_matches_official_literal_path_spelling() {
    // CC fsOperations.ts:166 compares JS strings, including trailing / and /.
    let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
    for suffix in ["/", "/."] {
        let spelling = format!("{}{suffix}", cwd.display());
        let result = safe_resolve_path(get_fs_implementation().as_ref(), Path::new(&spelling));
        assert!(result.is_symlink);
        assert_eq!(result.resolved_path, cwd);
    }
}

#[test]
fn safe_resolve_path_matches_official_nfc_realpath() {
    // CC FsOperations.realpathSync:523-525 normalizes before comparison.
    let root = std::env::temp_dir().join(format!("realpath-nfc-{}", uuid::Uuid::new_v4()));
    let decomposed = root.join("cafe\u{301}");
    std::fs::create_dir_all(&decomposed).unwrap();
    let raw = decomposed.canonicalize().unwrap();
    let expected = PathBuf::from(raw.to_string_lossy().nfc().collect::<String>());
    let result = safe_resolve_path(get_fs_implementation().as_ref(), &decomposed);
    assert_eq!(result.resolved_path.as_os_str(), expected.as_os_str());
    assert!(result.is_symlink);
    assert!(result.is_canonical);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn read_file_bytes_max_bytes_matches_official_prefix_read() {
    let path = std::env::temp_dir().join(format!(
        "nyan-read-file-bytes-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::write(&path, b"abcdef").unwrap();
    assert_eq!(read_file_bytes(&path, None).unwrap(), b"abcdef");
    assert_eq!(
        crate::utils::errors::io_errno_code(&read_file_bytes(&path, Some(3.9)).unwrap_err()),
        Some("ERR_OUT_OF_RANGE")
    );
    assert_eq!(read_file_bytes(&path, Some(20.0)).unwrap(), b"abcdef");
    let _ = std::fs::remove_file(path);
}

#[test]
fn bounded_ranges_matches_official_utf8_replacement() {
    let path = std::env::temp_dir().join(format!(
        "nyan-fs-range-unicode-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::write(&path, "😀abc😀").unwrap();
    let prefix = futures::executor::block_on(read_file_range(&path, 0, 2))
        .unwrap()
        .unwrap();
    assert_eq!(prefix.content, "�");
    assert_eq!(prefix.bytes_read, 2);
    assert_eq!(prefix.bytes_total, 11);
    let tail = futures::executor::block_on(tail_file(&path, 9)).unwrap();
    assert_eq!(tail.content, "��abc😀");
    let _ = std::fs::remove_file(path);
}

#[test]
fn atomic_replace_overwrites_existing_target() {
    let root = std::env::temp_dir().join(format!(
        "nyan-fs-atomic-replace-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let source = root.join("source");
    let target = root.join("target");
    std::fs::write(&source, "new").unwrap();
    std::fs::write(&target, "old").unwrap();
    replace_file_atomic(&source, &target).unwrap();
    assert!(!source.exists());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(windows)]
#[test]
fn windows_smoke_shared_delete_makes_open_file_remove_and_rename_predictable() {
    use std::os::windows::fs::OpenOptionsExt as _;

    let root = std::env::temp_dir().join(format!(
        "nyan-windows-shared-delete-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let fs = NodeFsOperations;

    let deleted = root.join("deleted.txt");
    std::fs::write(&deleted, "delete me").unwrap();
    let shared = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0x7)
        .open(&deleted)
        .unwrap();
    fs.unlink_sync(&deleted).unwrap();
    assert!(!deleted.exists());
    drop(shared);

    let source = root.join("source.txt");
    let target = root.join("renamed.txt");
    std::fs::write(&source, "rename me").unwrap();
    let shared = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0x7)
        .open(&source)
        .unwrap();
    fs.rename_sync(&source, &target).unwrap();
    assert!(!source.exists());
    assert!(target.exists());
    drop(shared);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "rename me");
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn permission_paths_include_parent_symlink_destination_for_new_files() {
    use std::os::unix::fs::symlink;
    let root = std::env::temp_dir().join(format!(
        "nyan-fs-permission-paths-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let logical = root.join("logical");
    let target = root.join("target");
    std::fs::create_dir_all(&target).unwrap();
    symlink(&target, &logical).unwrap();
    let input = logical.join("new.txt");
    let paths = get_paths_for_permission_check(&input);
    assert_eq!(paths.first(), Some(&input));
    let resolved_target = target.canonicalize().unwrap().join("new.txt");
    assert!(paths.iter().any(|path| path == &resolved_target));
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn bounded_readers_matches_official_follow_symlinks() {
    use std::os::unix::fs::symlink;

    let root = std::env::temp_dir().join(format!(
        "nyan-fs-range-symlink-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let victim = root.join("victim");
    let link = root.join("output");
    std::fs::write(&victim, "must-not-be-read").unwrap();
    symlink(&victim, &link).unwrap();
    assert_eq!(
        futures::executor::block_on(read_file_range(&link, 0, 1024))
            .unwrap()
            .unwrap()
            .content,
        "must-not-be-read"
    );
    assert_eq!(
        futures::executor::block_on(tail_file(&link, 1024))
            .unwrap()
            .content,
        "must-not-be-read"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn rm_matches_official_files_trees_force_and_symlinks() {
    // CC fsOperations.ts:410-412 delegates to fs/promises.rm. Fresh Bun
    // oracle: plugin-marketplace-git-0914/fs-rm-oracle.json. No trailing
    // separators/nonrecursive directories are asserted by this native slice.
    let root = std::env::temp_dir().join(format!("nyan-rm-{}", uuid::Uuid::new_v4()));
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    std::fs::create_dir_all(&root).unwrap();
    let _cleanup = Cleanup(root.clone());
    let file = root.join("file");
    std::fs::write(&file, "value").unwrap();
    rm(&file, false, false).await.unwrap();
    assert!(!file.exists());
    assert_eq!(
        rm(&file, true, false).await.unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    rm(&file, true, true).await.unwrap();
    let tree = root.join("tree");
    std::fs::create_dir_all(tree.join("nested")).unwrap();
    std::fs::write(tree.join("nested/value"), "value").unwrap();
    rm(&tree, true, false).await.unwrap();
    assert!(!tree.exists());
    std::fs::write(&file, "parent").unwrap();
    assert_eq!(
        rm(&file.join("child"), true, true)
            .await
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::NotADirectory
    );
    #[cfg(unix)]
    {
        let target = root.join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("kept"), "keep").unwrap();
        let link = root.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        rm(&link, true, false).await.unwrap();
        assert!(std::fs::symlink_metadata(&link).is_err());
        assert_eq!(
            std::fs::read_to_string(target.join("kept")).unwrap(),
            "keep"
        );
        std::os::unix::fs::symlink(root.join("missing"), &link).unwrap();
        rm(&link, true, false).await.unwrap();
        assert!(std::fs::symlink_metadata(&link).is_err());
    }
}
