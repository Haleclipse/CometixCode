//! Maps to: CC `utils/asciicast.ts` — asciicast v2 terminal recording.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;

use crate::bootstrap::state::{get_original_cwd, get_session_id};
use crate::utils::buffered_writer::{
    BufferedWriter, BufferedWriterOptions, create_buffered_writer,
};
use crate::utils::debug::log_for_debugging;
use crate::utils::env_utils::{get_claude_config_home_dir, is_env_truthy};
use crate::utils::session_storage::sanitize_path;

/// Maps to: CC `utils/asciicast.ts#recordingState:13-16`.
#[derive(Default)]
struct RecordingState {
    file_path: Option<PathBuf>,
    timestamp: u128,
}
static RECORDING_STATE: LazyLock<Mutex<RecordingState>> = LazyLock::new(Default::default);
static RECORDER: Mutex<Option<AsciicastRecorder>> = Mutex::new(None);

/// Maps to: CC `utils/asciicast.ts#getRecordFilePath:24-44`.
pub fn get_record_file_path() -> Option<PathBuf> {
    let mut state = RECORDING_STATE.lock().unwrap();
    if state.file_path.is_some() {
        return state.file_path.clone();
    }
    // Established compile-time distribution capability projection (PORTING.md).
    if !crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::Ui,
    ) || !is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_TERMINAL_RECORDING")
            .as_deref(),
    ) {
        return None;
    }
    state.timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    state.file_path = Some(
        get_claude_config_home_dir()
            .join("projects")
            .join(sanitize_path(&get_original_cwd().to_string_lossy()))
            .join(format!("{}-{}.cast", get_session_id(), state.timestamp)),
    );
    state.file_path.clone()
}

/// Maps to: CC `utils/asciicast.ts#_resetRecordingStateForTesting:46-49`.
pub fn _reset_recording_state_for_testing() {
    *RECORDING_STATE.lock().unwrap() = RecordingState::default();
}

/// Maps to: CC `utils/asciicast.ts#getSessionRecordingPaths:55-74`.
pub fn get_session_recording_paths() -> Vec<PathBuf> {
    let session_id = get_session_id();
    let project_dir = get_claude_config_home_dir()
        .join("projects")
        .join(sanitize_path(&get_original_cwd().to_string_lossy()));
    let Ok(entries) = fs::read_dir(&project_dir) else {
        return vec![];
    };
    let mut names = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { return vec![] };
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(&session_id) && name.ends_with(".cast") {
            names.push(name);
        }
    }
    names.sort_by_cached_key(|name| name.encode_utf16().collect::<Vec<_>>());
    names
        .into_iter()
        .map(|name| project_dir.join(name))
        .collect()
}

/// Maps to: CC `utils/asciicast.ts#renameRecordingForSession:82-109`.
/// Synchronous Rust filesystem carrier: flush completes before rename and the
/// caller cannot switch the transcript pointer until this function returns.
pub fn rename_recording_for_session() {
    let recorder = RECORDER.lock().unwrap();
    let (old_path, timestamp) = {
        let state = RECORDING_STATE.lock().unwrap();
        let Some(path) = state.file_path.clone() else {
            return;
        };
        if state.timestamp == 0 {
            return;
        }
        (path, state.timestamp)
    };
    let new_path = get_claude_config_home_dir()
        .join("projects")
        .join(sanitize_path(&get_original_cwd().to_string_lossy()))
        .join(format!("{}-{timestamp}.cast", get_session_id()));
    if new_path == old_path {
        return;
    }
    if let Some(recorder) = recorder.as_ref() {
        recorder.flush();
    }
    let old_name = old_path.file_name().unwrap_or_default().to_string_lossy();
    let new_name = new_path.file_name().unwrap_or_default().to_string_lossy();
    if fs::rename(&old_path, &new_path).is_ok() {
        RECORDING_STATE.lock().unwrap().file_path = Some(new_path.clone());
        log_for_debugging(&format!(
            "[asciicast] Renamed recording: {old_name} → {new_name}"
        ));
    } else {
        log_for_debugging(&format!(
            "[asciicast] Failed to rename recording from {old_name} to {new_name}"
        ));
    }
}

/// Maps to: CC `utils/asciicast.ts#AsciicastRecorder:111-114` and install closures.
struct AsciicastRecorder {
    writer: BufferedWriter,
    start_time: Instant,
    resize_task: Option<tokio::task::JoinHandle<()>>,
}
impl AsciicastRecorder {
    fn flush(&self) {
        let _ = self.writer.flush();
    }
    fn dispose(mut self) {
        let _ = self.writer.dispose();
        if let Some(task) = self.resize_task.take() {
            task.abort();
        }
    }
}

/// Maps to: CC `utils/asciicast.ts#getTerminalSize:118-125`.
fn get_terminal_size() -> (u16, u16) {
    // stdout dimensions, not crossterm's fallback /dev/tty or stderr size.
    #[cfg(unix)]
    {
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) } == 0 {
            return (
                if size.ws_col == 0 { 80 } else { size.ws_col },
                if size.ws_row == 0 { 24 } else { size.ws_row },
            );
        }
    }
    #[cfg(not(unix))]
    if let Ok((cols, rows)) = crossterm::terminal::size() {
        return (
            if cols == 0 { 80 } else { cols },
            if rows == 0 { 24 } else { rows },
        );
    }
    (80, 24)
}

/// Maps to: CC `utils/asciicast.ts#flushAsciicastRecorder:131-133`.
pub fn flush_asciicast_recorder() {
    if let Some(recorder) = RECORDER.lock().unwrap().as_ref() {
        recorder.flush();
    }
}

/// Maps to: CC `utils/asciicast.ts#installAsciicastRecorder:140-239`.
pub fn install_asciicast_recorder() -> io::Result<()> {
    let Some(file_path) = get_record_file_path() else {
        return Ok(());
    };
    let (cols, rows) = get_terminal_size();
    let start_time = Instant::now();
    let header = json!({"version":2,"width":cols,"height":rows,
        "timestamp":SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),
        "env":{"SHELL":crate::utils::process_env::var("SHELL").unwrap_or_default(),"TERM":crate::utils::process_env::var("TERM").unwrap_or_default()}});
    // fsOperations.ts#mkdirSync uses recursive:true (:528-537).
    if let Some(parent) = file_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    writeln!(options.open(&file_path)?, "{header}")?;
    let mut options = BufferedWriterOptions::new(Arc::new(|content| {
        let current_path = RECORDING_STATE.lock().unwrap().file_path.clone();
        if let Some(path) = current_path {
            // The buffered writer output mutex carries pendingWrite's serial
            // promise chain. File append failures never escape into stdout.
            let _ = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut file| file.write_all(content.as_bytes()));
        }
        Ok(())
    }));
    options.flush_interval_ms = 500;
    options.max_buffer_size = 50;
    options.max_buffer_bytes = Some(10 * 1024 * 1024);
    let writer = create_buffered_writer(options)?;
    let resize_task = None;
    *RECORDER.lock().unwrap() = Some(AsciicastRecorder {
        writer,
        start_time,
        resize_task,
    });
    #[cfg(unix)]
    {
        // Independent SIGWINCH subscription: never consumes keyboard events.
        let mut signal =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;
        let task = tokio::spawn(async move {
            // Runtime carrier for process.stdout.on('resize'): Bun emits only
            // when its TTY dimensions change, not for every raw SIGWINCH.
            // Verified with the live Kitty Bun probe (asciicast-0913).
            let mut previous_size = (cols, rows);
            while signal.recv().await.is_some() {
                let size = get_terminal_size();
                if size != previous_size {
                    previous_size = size;
                    on_resize(size);
                }
            }
        });
        RECORDER.lock().unwrap().as_mut().unwrap().resize_task = Some(task);
    }
    crate::utils::cleanup_registry::register_cleanup(|| async {
        dispose_asciicast_recorder();
    });
    log_for_debugging(&format!("[asciicast] Recording to {}", file_path.display()));
    Ok(())
}

/// Maps to: CC `utils/asciicast.ts#onResize:213-217`.
fn on_resize((cols, rows): (u16, u16)) {
    // The snapshot carries stdout.columns/rows already updated by the TTY
    // runtime, so an OS resize during a blocked flush cannot change this event.
    if let Some(recorder) = RECORDER.lock().unwrap().as_ref() {
        let _ = recorder.writer.write(&format!(
            "{}\n",
            json!([
                recorder.start_time.elapsed().as_secs_f64(),
                "r",
                format!("{cols}x{rows}")
            ])
        ));
    }
}

/// Rust cleanup entry for CC's registered dispose callback (:233-236).
pub fn dispose_asciicast_recorder() {
    if let Some(recorder) = RECORDER.lock().unwrap().take() {
        recorder.dispose();
    }
}

/// Write carrier for CC's process.stdout.write wrapper (:188-210).
/// Injected using iocraft's existing stdout API; does not replace the OS fd.
pub struct RecordingStdout<W>(pub W);
impl<W: Write> Write for RecordingStdout<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Some(recorder) = RECORDER.lock().unwrap().as_ref() {
            let _ = recorder.writer.write(&format!(
                "{}\n",
                json!([
                    recorder.start_time.elapsed().as_secs_f64(),
                    "o",
                    String::from_utf8_lossy(bytes)
                ])
            ));
        }
        // Node accepts the entire chunk; its bool is backpressure, not a
        // Rust short-write count. Drain the carrier without re-recording tails.
        self.0.write_all(bytes)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use serde_json::Value;

    struct Fixture {
        root: PathBuf,
        _config: EnvVarGuard,
        cwd: PathBuf,
        session: String,
        project: Option<PathBuf>,
    }
    impl Fixture {
        fn new() -> Self {
            dispose_asciicast_recorder();
            _reset_recording_state_for_testing();
            let root =
                std::env::temp_dir().join(format!("cometix-asciicast-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&root).unwrap();
            let config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", root.to_str().unwrap());
            let cwd = get_original_cwd();
            let session = get_session_id();
            let project = crate::bootstrap::state::get_session_project_dir();
            crate::bootstrap::state::set_original_cwd("/oracle");
            crate::bootstrap::state::switch_session("source".to_string(), None);
            Self {
                root,
                _config: config,
                cwd,
                session,
                project,
            }
        }
        fn seed_path(&self) -> PathBuf {
            let path = get_claude_config_home_dir().join("projects/-oracle/source-100000.cast");
            *RECORDING_STATE.lock().unwrap() = RecordingState {
                file_path: Some(path.clone()),
                timestamp: 100000,
            };
            path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            dispose_asciicast_recorder();
            _reset_recording_state_for_testing();
            let _ = fs::remove_dir_all(&self.root);
            crate::bootstrap::state::set_original_cwd(self.cwd.clone());
            crate::bootstrap::state::switch_session(self.session.clone(), self.project.clone());
        }
    }
    struct ShortWriter(Vec<u8>);
    impl Write for ShortWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let length = bytes.len().min(2);
            self.0.extend_from_slice(&bytes[..length]);
            Ok(length)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn rows(path: &std::path::Path) -> Vec<Value> {
        fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn recording_paths_matches_official_gates_cache_prefix_sort_and_missing_directory() {
        // CC asciicast.ts:24-74: non-null cache precedes gates; directory
        // enumeration has no recording gate and matches prefix, not UUID equality.
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let fixture = Fixture::new();
        let _off = EnvVarGuard::set("CLAUDE_CODE_TERMINAL_RECORDING", "0");
        assert_eq!(get_record_file_path(), None);
        assert!(get_session_recording_paths().is_empty());
        let path = fixture.seed_path();
        assert_eq!(get_record_file_path(), Some(path.clone()));
        let dir = path.parent().unwrap();
        fs::create_dir_all(dir).unwrap();
        for name in [
            "source-20.cast",
            "source-10.cast",
            "sourceSuffix.cast",
            "other.cast",
            "source.txt",
        ] {
            fs::write(dir.join(name), "").unwrap();
        }
        assert_eq!(
            get_session_recording_paths(),
            vec![
                dir.join("source-10.cast"),
                dir.join("source-20.cast"),
                dir.join("sourceSuffix.cast")
            ]
        );
        _reset_recording_state_for_testing();
        let _on = EnvVarGuard::set("CLAUDE_CODE_TERMINAL_RECORDING", "1");
        assert_eq!(
            get_record_file_path().is_some(),
            crate::utils::build_profile::build_audience().is_internal()
        );
    }

    #[test]
    fn recorder_matches_official_bun_output_resize_rename_flush_and_dispose() {
        // Original asciicast.ts executed with actual bufferedWriter under Bun;
        // oracle: ANSI output, lossy byte chunk, resize, rename, output, dispose.
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let fixture = Fixture::new();
        let old_path = fixture.seed_path();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _enter = runtime.enter();
        install_asciicast_recorder().unwrap();
        let mut output = RecordingStdout(ShortWriter(Vec::new()));
        output.write_all(b"\x1b[31mhello\x1b[0m").unwrap();
        output.write_all(&[0xff, b'a']).unwrap();
        // Snapshot supplied by stdout resize, deliberately unlike live ioctl.
        on_resize((100, 40));
        crate::bootstrap::state::switch_session("target".to_string(), None);
        rename_recording_for_session();
        let path = get_record_file_path().unwrap();
        assert_eq!(path.file_name().unwrap(), "target-100000.cast");
        assert!(!old_path.exists());
        output.write_all(b"after").unwrap();
        flush_asciicast_recorder();
        let recorded = rows(&path);
        assert_eq!(recorded.len(), 5);
        assert_eq!(recorded[0]["version"], 2);
        let (cols, r) = get_terminal_size();
        assert_eq!(recorded[0]["width"], cols);
        assert_eq!(recorded[0]["height"], r);
        assert_eq!(recorded[1][2], "\x1b[31mhello\x1b[0m");
        assert_eq!(recorded[2][2], "�a");
        assert_eq!(recorded[3][1], "r");
        assert_eq!(recorded[3][2], "100x40");
        assert_eq!(recorded[4][2], "after");
        assert!(
            recorded[1..]
                .windows(2)
                .all(|pair| pair[0][0].as_f64().unwrap() <= pair[1][0].as_f64().unwrap())
        );
        assert_eq!(get_session_recording_paths(), vec![path.clone()]);
        dispose_asciicast_recorder();
        output.write_all(b"unrecorded").unwrap();
        assert_eq!(rows(&path), recorded);
        assert_eq!(
            output.0.0,
            [
                b"\x1b[31mhello\x1b[0m".as_slice(),
                &[0xff, b'a'],
                b"after",
                b"unrecorded"
            ]
            .concat()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn recorder_matches_official_initial_error_rename_error_and_silent_append_error() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let fixture = Fixture::new();
        let path = fixture.seed_path();
        fs::create_dir_all(&path).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _enter = runtime.enter();
        assert!(install_asciicast_recorder().is_err());
        assert!(RECORDER.lock().unwrap().is_none());
        fs::remove_dir(&path).unwrap();
        install_asciicast_recorder().unwrap();
        fs::remove_file(&path).unwrap();
        crate::bootstrap::state::switch_session("target".to_string(), None);
        rename_recording_for_session();
        assert_eq!(get_record_file_path(), Some(path.clone()));
        fs::create_dir(&path).unwrap();
        let mut output = RecordingStdout(ShortWriter(Vec::new()));
        output.write_all(b"survives disk errors").unwrap();
        flush_asciicast_recorder();
        dispose_asciicast_recorder();
        assert_eq!(output.0.0, b"survives disk errors");
    }
}
