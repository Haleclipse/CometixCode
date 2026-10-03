//! Disk-backed task output paths.
//!
//! Maps to CC `utils/task/diskOutput.ts`.
//!
//! Cometix keeps the same task-output directory and output-file seam used by
//! background shell/agent tasks. The official implementation uses an async
//! write queue; this Rust owner performs bounded synchronous appends while
//! preserving the same retained-file lifecycle and fail-closed no-write seam.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, RwLock};

pub const MAX_TASK_OUTPUT_BYTES: u64 = 5 * 1024 * 1024 * 1024;
pub const MAX_TASK_OUTPUT_BYTES_DISPLAY: &str = "5GB";
const DEFAULT_MAX_READ_BYTES: u64 = 8 * 1024 * 1024;

static TASK_OUTPUT_DIR: LazyLock<RwLock<Option<PathBuf>>> = LazyLock::new(|| RwLock::new(None));
static INTENTIONAL_OUTPUT_SYMLINKS: LazyLock<Mutex<HashMap<PathBuf, PathBuf>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Maps to CC `diskOutput.ts#getTaskOutputDir`.
pub fn get_task_output_dir() -> PathBuf {
    if let Ok(read) = TASK_OUTPUT_DIR.read() {
        if let Some(path) = read.as_ref() {
            return path.clone();
        }
    }

    let path = crate::utils::permissions::filesystem::get_project_temp_dir()
        .join(crate::bootstrap::state::get_session_id())
        .join("tasks");
    if let Ok(mut write) = TASK_OUTPUT_DIR.write() {
        if let Some(existing) = write.as_ref() {
            return existing.clone();
        }
        *write = Some(path.clone());
    }
    path
}

/// Maps to CC `diskOutput.ts#_resetTaskOutputDirForTest`.
#[cfg(test)]
pub fn reset_task_output_dir_for_test() {
    if let Ok(mut write) = TASK_OUTPUT_DIR.write() {
        *write = None;
    }
}

/// Maps to CC `diskOutput.ts#getTaskOutputPath`.
pub fn get_task_output_path(task_id: &str) -> PathBuf {
    get_task_output_dir().join(format!("{task_id}.output"))
}

pub(crate) fn trim_partial_utf8_boundaries(bytes: &mut Vec<u8>, trim_leading: bool) -> usize {
    let mut skipped = 0usize;
    if trim_leading {
        skipped = bytes
            .iter()
            .take_while(|byte| **byte & 0b1100_0000 == 0b1000_0000)
            .count();
        if skipped > 0 {
            bytes.drain(..skipped);
        }
    }
    if bytes.is_empty() {
        return skipped;
    }
    let mut lead = bytes.len() - 1;
    while lead > 0 && bytes[lead] & 0b1100_0000 == 0b1000_0000 {
        lead -= 1;
    }
    let width = match bytes[lead] {
        0x00..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => 1,
    };
    if width > bytes.len() - lead {
        bytes.truncate(lead);
    }
    skipped
}

fn ensure_output_dir() -> std::io::Result<()> {
    std::fs::create_dir_all(get_task_output_dir())
}

/// Maps to CC `diskOutput.ts#initTaskOutput`.
pub fn init_task_output(task_id: &str) -> std::io::Result<PathBuf> {
    ensure_output_dir()?;
    let path = get_task_output_path(task_id);
    INTENTIONAL_OUTPUT_SYMLINKS.lock().unwrap().remove(&path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    match options.open(&path) {
        Ok(_) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "refusing unsafe pre-existing task-output path",
                ));
            }
            Ok(path)
        }
        Err(error) => Err(error),
    }
}

/// Maps to CC `diskOutput.ts#initTaskOutputAsSymlink`.
pub fn init_task_output_as_symlink(
    task_id: &str,
    target_path: PathBuf,
) -> std::io::Result<PathBuf> {
    // Official local-agent task outputs are symlinks to the sidechain JSONL.
    // In explicit no-write mode, preserve a readable inert file instead of
    // creating either the link or its target.
    if !crate::utils::session_storage::is_session_write_enabled() {
        return init_task_output(task_id);
    }
    let target_path = if target_path.is_absolute() {
        target_path
    } else {
        std::env::current_dir()?.join(target_path)
    };

    ensure_output_dir()?;
    let output_path = get_task_output_path(task_id);
    INTENTIONAL_OUTPUT_SYMLINKS
        .lock()
        .unwrap()
        .remove(&output_path);
    #[cfg(unix)]
    {
        match std::os::unix::fs::symlink(&target_path, &output_path) {
            Ok(()) => {}
            Err(_) => {
                let _ = std::fs::remove_file(&output_path);
                std::os::unix::fs::symlink(&target_path, &output_path)?;
            }
        }
        INTENTIONAL_OUTPUT_SYMLINKS
            .lock()
            .unwrap()
            .insert(output_path.clone(), target_path);
        Ok(output_path)
    }
    #[cfg(windows)]
    {
        match std::os::windows::fs::symlink_file(&target_path, &output_path) {
            Ok(()) => {}
            Err(_) => {
                let _ = std::fs::remove_file(&output_path);
                std::os::windows::fs::symlink_file(&target_path, &output_path)?;
            }
        }
        INTENTIONAL_OUTPUT_SYMLINKS
            .lock()
            .unwrap()
            .insert(output_path.clone(), target_path);
        Ok(output_path)
    }
}

fn output_path_for_io(output_path: &Path) -> std::io::Result<PathBuf> {
    let Ok(metadata) = std::fs::symlink_metadata(output_path) else {
        return Ok(output_path.to_path_buf());
    };
    if !metadata.file_type().is_symlink() {
        return Ok(output_path.to_path_buf());
    }
    let expected = INTENTIONAL_OUTPUT_SYMLINKS
        .lock()
        .unwrap()
        .get(output_path)
        .cloned()
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "refusing unregistered task-output symlink",
            )
        })?;
    let actual = std::fs::read_link(output_path)?;
    if actual != expected {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "task-output symlink target changed after registration",
        ));
    }
    Ok(expected)
}

fn open_task_output_for_read(path: &Path) -> std::io::Result<std::fs::File> {
    let path = output_path_for_io(path)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

/// Maps to CC `diskOutput.ts#appendTaskOutput`.
pub fn append_task_output(task_id: &str, content: &str) -> std::io::Result<()> {
    if !crate::utils::session_storage::is_session_write_enabled() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "task output persistence is disabled by COMETIX_WRITE_ENABLED=0",
        ));
    }
    ensure_output_dir()?;
    let output_path = get_task_output_path(task_id);
    let path = output_path_for_io(&output_path)?;
    let current_len = std::fs::metadata(&path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(&path)?;
    if current_len >= MAX_TASK_OUTPUT_BYTES {
        return Ok(());
    }
    let remaining = (MAX_TASK_OUTPUT_BYTES - current_len) as usize;
    if content.len() > remaining {
        // CC drops the chunk that crosses the cap and writes one terminal
        // marker; it does not retain a partial UTF-8 fragment of that chunk.
        file.write_all(
            format!("\n[output truncated: exceeded {MAX_TASK_OUTPUT_BYTES_DISPLAY} disk cap]\n")
                .as_bytes(),
        )?;
    } else {
        file.write_all(content.as_bytes())?;
    }
    Ok(())
}

/// Maps to CC `diskOutput.ts#flushTaskOutput`.
pub async fn flush_task_output(_task_id: &str) -> std::io::Result<()> {
    Ok(())
}

/// Maps to CC `diskOutput.ts#evictTaskOutput`.
pub async fn evict_task_output(_task_id: &str) -> std::io::Result<()> {
    Ok(())
}

/// Maps to CC `diskOutput.ts#getTaskOutput`.
pub fn get_task_output(task_id: &str, max_bytes: u64) -> String {
    let max_bytes = if max_bytes == 0 {
        DEFAULT_MAX_READ_BYTES
    } else {
        max_bytes
    } as usize;
    let path = get_task_output_path(task_id);
    let Ok(mut file) = open_task_output_for_read(&path) else {
        return String::new();
    };
    let Ok(total) = file.metadata().map(|metadata| metadata.len()) else {
        return String::new();
    };
    let start = total.saturating_sub(max_bytes as u64);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut bytes = Vec::with_capacity((total - start) as usize);
    if file.read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    let boundary_skip = trim_partial_utf8_boundaries(&mut bytes, start > 0);
    if start == 0 {
        return String::from_utf8_lossy(&bytes).to_string();
    }
    format!(
        "[{}KB of earlier output omitted]\n{}",
        (start + boundary_skip as u64) / 1024,
        String::from_utf8_lossy(&bytes)
    )
}

/// Maps to CC `diskOutput.ts#getTaskOutputSize`.
pub fn get_task_output_size(task_id: &str) -> u64 {
    output_path_for_io(&get_task_output_path(task_id))
        .and_then(std::fs::metadata)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

/// Maps to CC `diskOutput.ts#cleanupTaskOutput`.
pub fn cleanup_task_output(task_id: &str) -> std::io::Result<()> {
    let path = get_task_output_path(task_id);
    INTENTIONAL_OUTPUT_SYMLINKS.lock().unwrap().remove(&path);
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    struct TaskOutputDirGuard(Option<PathBuf>);

    impl TaskOutputDirGuard {
        fn isolate() -> Self {
            INTENTIONAL_OUTPUT_SYMLINKS.lock().unwrap().clear();
            let previous = TASK_OUTPUT_DIR
                .write()
                .map(|mut path| path.take())
                .unwrap_or_default();
            Self(previous)
        }
    }

    impl Drop for TaskOutputDirGuard {
        fn drop(&mut self) {
            INTENTIONAL_OUTPUT_SYMLINKS.lock().unwrap().clear();
            if let Ok(mut path) = TASK_OUTPUT_DIR.write() {
                *path = self.0.take();
            }
        }
    }

    #[test]
    fn task_output_path_uses_official_project_session_tasks_shape() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _dir = TaskOutputDirGuard::isolate();
        let path = get_task_output_path("task-abc");
        assert!(path.ends_with(std::path::Path::new("tasks").join("task-abc.output")));
        assert!(
            path.display()
                .to_string()
                .contains(&crate::bootstrap::state::get_session_id())
        );
    }

    #[test]
    fn append_and_tail_task_output_matches_official_file_boundary() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _dir = TaskOutputDirGuard::isolate();
        let task_id = format!("task-{}", uuid::Uuid::new_v4());
        let path = init_task_output(&task_id).unwrap();
        append_task_output(&task_id, "hello\nworld").unwrap();
        assert_eq!(get_task_output(&task_id, 1024), "hello\nworld");
        assert_eq!(get_task_output_size(&task_id), 11);
        cleanup_task_output(&task_id).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn tail_reads_do_not_emit_partial_utf8_codepoints() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _dir = TaskOutputDirGuard::isolate();
        let task_id = format!("task-{}", uuid::Uuid::new_v4());
        let path = init_task_output(&task_id).unwrap();
        std::fs::write(&path, "😀abc😀").unwrap();
        let tail = get_task_output(&task_id, 9);
        assert!(tail.ends_with("abc😀"));
        assert!(!tail.contains('\u{fffd}'));
        cleanup_task_output(&task_id).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn append_task_output_refuses_symlink_targets() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _dir = TaskOutputDirGuard::isolate();
        let task_id = format!("task-{}", uuid::Uuid::new_v4());
        let output_path = get_task_output_path(&task_id);
        std::fs::create_dir_all(output_path.parent().unwrap()).unwrap();
        let victim = output_path.with_extension("victim");
        std::fs::write(&victim, "safe").unwrap();
        std::os::unix::fs::symlink(&victim, &output_path).unwrap();

        assert!(init_task_output(&task_id).is_err());
        assert!(append_task_output(&task_id, "attacker").is_err());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "safe");
        let _ = std::fs::remove_file(output_path);
        let _ = std::fs::remove_file(victim);
    }

    #[cfg(unix)]
    #[test]
    fn registered_agent_symlink_remains_readable_and_appendable() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _dir = TaskOutputDirGuard::isolate();
        let task_id = format!("agent-{}", uuid::Uuid::new_v4());
        let target_dir = std::env::temp_dir().join(format!(
            "cometix-agent-output-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&target_dir).unwrap();
        let target = target_dir.join("transcript.jsonl");
        std::fs::write(&target, "first\n").unwrap();
        let output_path = init_task_output_as_symlink(&task_id, target.clone()).unwrap();

        append_task_output(&task_id, "second\n").unwrap();
        assert_eq!(get_task_output(&task_id, 1024), "first\nsecond\n");
        assert_eq!(get_task_output_size(&task_id), 13);
        cleanup_task_output(&task_id).unwrap();
        assert!(!output_path.exists());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "first\nsecond\n");
        let _ = std::fs::remove_dir_all(target_dir);
    }

    #[test]
    fn agent_symlink_initialization_falls_back_to_real_file_while_session_writes_disabled() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "0");
        let _dir = TaskOutputDirGuard::isolate();
        let task_id = format!("agent-{}", uuid::Uuid::new_v4());
        let path =
            init_task_output_as_symlink(&task_id, PathBuf::from("/no/such/transcript")).unwrap();
        assert!(path.exists());
        let error = append_task_output(&task_id, "done").unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(get_task_output(&task_id, 1024).is_empty());
        cleanup_task_output(&task_id).unwrap();
    }
}
