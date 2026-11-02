//! Native Windows dependency regressions for CC fsOperations.ts:509–516.
//! Node 24.14 fs.js#symlinkSync and internal/fs/utils.js#preprocessSymlinkDestination.
use super::{FsOperations, NodeFsOperations, SymlinkType};
use std::path::Path;

#[test]
fn windows_smoke_junction_matches_official_node_link_parent_resolution() {
    let root = std::env::temp_dir().join(format!("node-junction-{}", uuid::Uuid::new_v4()));
    let parent = root.join("links");
    let target = parent.join("assets");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("sentinel"), b"link parent").unwrap();
    let link = parent.join("shortcut");
    let fs = NodeFsOperations;
    fs.symlink_sync(Path::new("assets"), &link, Some(SymlinkType::Junction))
        .unwrap();
    // Process cwd is deliberately unrelated to the link's parent.
    assert_eq!(
        std::fs::read(link.join("sentinel")).unwrap(),
        b"link parent"
    );
    assert_eq!(
        fs.realpath_sync(&link).unwrap(),
        fs.realpath_sync(&target).unwrap()
    );
    std::fs::remove_dir(&link).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_smoke_symlink_detection_is_optional_without_symbolic_link_privilege() {
    let root = std::env::temp_dir().join(format!("node-symlink-{}", uuid::Uuid::new_v4()));
    let parent = root.join("links");
    std::fs::create_dir_all(parent.join("assets")).unwrap();
    std::fs::write(parent.join("assets/sentinel"), b"directory").unwrap();
    std::fs::write(parent.join("file"), b"file").unwrap();
    let fs = NodeFsOperations;
    let dir_link = parent.join("dir-link");
    let file_link = parent.join("file-link");
    let missing_link = parent.join("missing-link");
    let notdir_link = parent.join("notdir-link");
    if let Err(error) = fs.symlink_sync(Path::new("file"), &file_link, Some(SymlinkType::File)) {
        eprintln!(
            "windows_smoke_file_symlink: skipped because the runner cannot create file symlinks: {error}"
        );
        let _ = std::fs::remove_dir_all(root);
        return;
    }
    fs.symlink_sync(Path::new("assets"), &dir_link, None)
        .unwrap();
    fs.symlink_sync(Path::new("missing"), &missing_link, None)
        .unwrap();
    assert_eq!(
        std::fs::read(dir_link.join("sentinel")).unwrap(),
        b"directory"
    );
    assert_eq!(std::fs::read(&file_link).unwrap(), b"file");
    assert!(
        fs.lstat_sync(&missing_link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(!fs.exists_sync(&missing_link));
    fs.symlink_sync(Path::new("file/child"), &notdir_link, None)
        .unwrap();
    assert!(
        fs.lstat_sync(&notdir_link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    std::fs::remove_file(notdir_link).unwrap();
    std::fs::remove_dir(dir_link).unwrap();
    std::fs::remove_file(file_link).unwrap();
    std::fs::remove_file(missing_link).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_link_preparation_matches_official_node_names() {
    for (input, expected) in [
        ("dir/file", r"dir\file"),
        (r"C:\base\..\file", r"\\?\C:\file"),
        (r"\\server\share\file", r"\\?\UNC\server\share\file"),
        (r"\\?\C:\base\..\file", r"\\?\C:\file"),
    ] {
        assert_eq!(
            super::windows::symlink_destination(Path::new(input))
                .unwrap()
                .as_os_str(),
            expected
        );
    }
    for (input, substitute, display) in [
        (r"\\?\C:\target", r"\??\C:\target", r"C:\target"),
        (
            r"\\?\UNC\server\share\",
            r"\??\UNC\server\share\",
            r"UNC\server\share\",
        ),
        (r"\\?\C:\a\\b\", r"\??\C:\a\b\", r"C:\a\b\"),
    ] {
        let names = super::windows::junction_names(Path::new(input)).unwrap();
        assert_eq!(names.0, substitute.encode_utf16().collect::<Vec<_>>());
        assert_eq!(names.1, display.encode_utf16().collect::<Vec<_>>());
    }
    assert!(super::windows::junction_names(Path::new(r"\\.\pipe\name")).is_err());
    use std::os::windows::ffi::OsStringExt;
    let input = std::ffi::OsString::from_wide(&[92, 92, 63, 92, 67, 58, 92, 0xd800]);
    let (_, display) = super::windows::junction_names(Path::new(&input)).unwrap();
    assert_eq!(display, [67, 58, 92, 0xd800]);
}

#[test]
fn windows_junction_rejects_nul_target_matches_official_node() {
    let link = std::env::temp_dir().join(format!("node-junction-nul-{}", uuid::Uuid::new_v4()));
    let error = NodeFsOperations
        .symlink_sync(
            Path::new("target\0tail"),
            &link,
            Some(SymlinkType::Junction),
        )
        .unwrap_err();
    assert_eq!(
        crate::utils::errors::io_errno_code(&error),
        Some("ERR_INVALID_ARG_VALUE")
    );
    assert!(!link.exists());
}
