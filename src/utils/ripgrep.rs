//! Maps to: CC `utils/ripgrep.ts`.
//!
//! GrepTool / GlobalSearch / telemetry shell out to `rg`. The packaged vendor
//! is the default; PATH `rg` is selected only by the official env override.

use crate::tool::AbortController;
use crate::utils::debug::log_for_debugging;
use crate::utils::process_env::JsTruthy;
use std::collections::HashMap;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

const MAX_BUFFER_SIZE: usize = 20_000_000;
const EAGAIN_STDERR_RETRY_PREFIX: &str = "\0cometix-ripgrep-stderr-eagain:";

/// Maps to CC `RipgrepConfig.mode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RipgrepMode {
    System,
    Builtin,
    Embedded,
}

impl RipgrepMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Builtin => "builtin",
            Self::Embedded => "embedded",
        }
    }
}

#[derive(Clone, Debug)]
struct RipgrepConfig {
    mode: RipgrepMode,
    command: String,
    args: Vec<String>,
    argv0: Option<String>,
}

/// Public shape returned by [`ripgrep_command`] (CC `ripgrepCommand()`).
#[derive(Clone, Debug)]
pub struct RipgrepCommand {
    pub rg_path: String,
    pub rg_args: Vec<String>,
    pub argv0: Option<String>,
}

/// Maps to CC `getRipgrepStatus()` return type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RipgrepStatusInfo {
    pub mode: RipgrepMode,
    pub path: String,
    /// `None` until [`test_ripgrep_on_first_use`] has run.
    pub working: Option<bool>,
}

struct RipgrepAvailability {
    working: bool,
    #[allow(dead_code)]
    last_tested_ms: u128,
    config: RipgrepConfig,
}

static RIPGREP_CONFIG: OnceLock<RipgrepConfig> = OnceLock::new();
static RIPGREP_STATUS: Mutex<Option<RipgrepAvailability>> = Mutex::new(None);
static RIPGREP_PROBE_STARTED: AtomicBool = AtomicBool::new(false);
static ALREADY_DONE_SIGN_CHECK: Mutex<bool> = Mutex::new(false);
static COUNT_FILES_CACHE: Mutex<Option<HashMap<String, Option<u64>>>> = Mutex::new(None);

fn vendor_ripgrep_platform_dir() -> String {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    format!("{arch}-{platform}")
}

/// Locate the packaged ripgrep, the way CC's sibling package layout does.
///
/// CC ships `vendor/ripgrep/<platform>/rg` as a file *inside* the npm package
/// and executes that path; the binary is packaged alongside the program, never
/// compiled into it. This keeps the same shape: the binary is an external
/// asset, looked up next to the executable and then in the development tree.
///
/// `None` means no packaged copy is present, which is the caller's signal to
/// fall back to a host `rg`.
fn vendor_rg_path() -> Option<PathBuf> {
    let dir = vendor_ripgrep_platform_dir();
    let bin = if cfg!(windows) { "rg.exe" } else { "rg" };
    // Release archives ship CC's sibling package layout directly.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let candidate = parent.join("vendor").join("ripgrep").join(&dir).join(bin);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    // Development tree: the checked-out vendor directory.
    let in_tree = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("vendor")
        .join("ripgrep")
        .join(dir)
        .join(bin);
    in_tree.is_file().then_some(in_tree)
}

/// Maps to CC `getRipgrepConfig` (memoized).
fn get_ripgrep_config() -> RipgrepConfig {
    RIPGREP_CONFIG
        .get_or_init(|| {
            // USE_BUILTIN_RIPGREP falsy → prefer system `rg`.
            if crate::utils::env_utils::is_env_defined_falsy(
                crate::utils::process_env::var("USE_BUILTIN_RIPGREP").as_deref(),
            ) && system_rg_path().is_some()
            {
                return RipgrepConfig {
                    mode: RipgrepMode::System,
                    // SECURITY: command name, not absolute path (CC system mode).
                    command: "rg".to_string(),
                    args: Vec::new(),
                    argv0: None,
                };
            }

            // Embedded (argv0='rg') is Bun-only upstream: the single-file
            // executable re-invokes itself as rg. Nothing here constructs that
            // mode; the variant is kept and left empty on purpose, reserved
            // for a release build that ships rg inside the binary.

            // Prefer the packaged standalone binary, as CC does.
            if let Some(path) = vendor_rg_path() {
                return RipgrepConfig {
                    mode: RipgrepMode::Builtin,
                    command: path.display().to_string(),
                    args: Vec::new(),
                    argv0: None,
                };
            }

            // @cometix deviation: CC cannot reach this branch — npm ships the
            // binary inside the package, so CC surfaces a missing one as
            // ENOENT rather than silently switching to a host ripgrep with
            // possibly different semantics. A `cargo install` build has no
            // sibling vendor directory and no source tree, so falling through
            // to ENOENT would break every search for anyone who installed that
            // way. Prefer the host `rg` instead; `get_ripgrep_status` reports
            // the mode, so the substitution stays visible in /doctor.
            RipgrepConfig {
                mode: RipgrepMode::System,
                // SECURITY: command name, not absolute path (CC system mode).
                command: "rg".to_string(),
                args: Vec::new(),
                argv0: None,
            }
        })
        .clone()
}

/// Maps to CC `ripgrepCommand()`.
pub fn ripgrep_command() -> RipgrepCommand {
    let config = get_ripgrep_config();
    RipgrepCommand {
        rg_path: config.command,
        rg_args: config.args,
        argv0: config.argv0,
    }
}

/// Maps to CC `getRipgrepStatus()`.
pub fn get_ripgrep_status() -> RipgrepStatusInfo {
    let config = get_ripgrep_config();
    let working = RIPGREP_STATUS
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().map(|status| status.working));
    RipgrepStatusInfo {
        mode: config.mode,
        path: config.command,
        working,
    }
}

fn parse_timeout_seconds(value: &str) -> u64 {
    let trimmed = value.trim_start();
    let (negative, digits) = match trimmed.as_bytes().first() {
        Some(b'+') => (false, &trimmed[1..]),
        Some(b'-') => (true, &trimmed[1..]),
        _ => (false, trimmed),
    };
    let digit_count = digits
        .as_bytes()
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if negative || digit_count == 0 {
        return 0;
    }
    digits[..digit_count].parse::<u64>().unwrap_or(u64::MAX)
}

fn platform_default_timeout_seconds() -> u64 {
    use crate::utils::platform::{Platform, get_platform};
    if get_platform() == Platform::Wsl {
        60
    } else {
        20
    }
}

fn default_timeout() -> Duration {
    let parsed_seconds = crate::utils::process_env::var("CLAUDE_CODE_GLOB_TIMEOUT_SECONDS")
        .truthy()
        .map(|value| parse_timeout_seconds(&value))
        .unwrap_or(0);
    if parsed_seconds > 0 {
        return Duration::from_secs(parsed_seconds);
    }
    Duration::from_secs(platform_default_timeout_seconds())
}

fn lines_from_stdout(stdout: &str) -> Vec<String> {
    stdout
        .trim()
        .split('\n')
        .map(|line| line.trim_end_matches('\r').to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

fn drain_ripgrep_pipe<R: Read>(
    mut pipe: R,
    max_buffer_size: usize,
    buffer_overflow: Arc<AtomicBool>,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut retained = Vec::new();
    let mut truncated = false;
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let read = pipe.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        let remaining = max_buffer_size.saturating_sub(retained.len());
        retained.extend_from_slice(&chunk[..read.min(remaining)]);
        if read > remaining {
            truncated = true;
            buffer_overflow.store(true, Ordering::Release);
        }
        // Keep draining until the owner kills/closes the process. Embedded CC
        // intentionally drains after its retained cap; execFile mode is killed
        // promptly by the polling owner when this signal becomes visible.
    }
    Ok((retained, truncated))
}

fn join_ripgrep_pipe(
    reader: std::thread::JoinHandle<std::io::Result<(Vec<u8>, bool)>>,
    stream: &str,
) -> Result<(String, bool), String> {
    let (bytes, truncated) = reader
        .join()
        .map_err(|_| format!("ripgrep {stream} reader panicked"))?
        .map_err(|error| format!("Error reading ripgrep {stream}: {error}"))?;
    Ok((String::from_utf8_lossy(&bytes).into_owned(), truncated))
}

fn is_egain_error(stderr: &str) -> bool {
    stderr.contains("os error 11") || stderr.contains("Resource temporarily unavailable")
}

fn count_char_in_bytes(buf: &[u8], target: u8) -> usize {
    buf.iter().filter(|byte| **byte == target).count()
}

fn resolve_spawn_command(config: &RipgrepConfig) -> String {
    // SECURITY: preserve CC's command-name execution in system mode rather
    // than substituting a previously resolved absolute path.
    config.command.clone()
}

/// Maps to CC `codesignRipgrepIfNecessary` — macos + builtin vendor only.
pub fn codesign_ripgrep_if_necessary() {
    if !cfg!(target_os = "macos") {
        return;
    }
    let Ok(mut done) = ALREADY_DONE_SIGN_CHECK.lock() else {
        return;
    };
    if *done {
        return;
    }
    *done = true;
    drop(done);

    let config = get_ripgrep_config();
    if config.mode != RipgrepMode::Builtin {
        return;
    }
    let builtin_path = config.command;

    // CC `execFileNoThrow` inherits process.env (execa default); the carrier
    // is its counterpart. Same for the sign and xattr spawns below.
    let mut probe_command = Command::new("codesign");
    crate::utils::subprocess_env::apply_process_env_std(&mut probe_command);
    let probe = probe_command.args(["-vv", "-d", &builtin_path]).output();
    let Ok(probe) = probe else {
        return;
    };
    let stdout = String::from_utf8_lossy(&probe.stdout);
    let needs_signed = stdout.lines().any(|line| line.contains("linker-signed"));
    if !needs_signed {
        return;
    }

    let mut sign_command = Command::new("codesign");
    crate::utils::subprocess_env::apply_process_env_std(&mut sign_command);
    let sign = sign_command
        .args([
            "--sign",
            "-",
            "--force",
            "--preserve-metadata=entitlements,requirements,flags,runtime",
            &builtin_path,
        ])
        .output();
    if let Ok(sign) = sign {
        if !sign.status.success() {
            log_for_debugging(&format!(
                "Failed to sign ripgrep: {} {}",
                String::from_utf8_lossy(&sign.stdout),
                String::from_utf8_lossy(&sign.stderr)
            ));
        }
    }

    let mut quarantine_command = Command::new("xattr");
    crate::utils::subprocess_env::apply_process_env_std(&mut quarantine_command);
    let quarantine = quarantine_command
        .args(["-d", "com.apple.quarantine", &builtin_path])
        .output();
    if let Ok(quarantine) = quarantine {
        if !quarantine.status.success() {
            log_for_debugging(&format!(
                "Failed to remove quarantine: {} {}",
                String::from_utf8_lossy(&quarantine.stdout),
                String::from_utf8_lossy(&quarantine.stderr)
            ));
        }
    }
}

fn run_ripgrep_first_use_probe(config: &RipgrepConfig) -> bool {
    let command = resolve_spawn_command(config);
    let mut args = config.args.clone();
    args.push("--version".to_string());
    let mut rg_command = Command::new(&command);
    // CC `execFileNoThrow` / `Bun.spawn` inherit process.env; the carrier is its counterpart.
    crate::utils::subprocess_env::apply_process_env_std(&mut rg_command);
    let mut child = match rg_command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            log_for_debugging(&format!("Ripgrep first use test error: {error}"));
            return false;
        }
    };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = Vec::new();
                if let Some(mut pipe) = child.stdout.take() {
                    let _ = pipe.read_to_end(&mut stdout);
                }
                return status.success() && stdout.starts_with(b"ripgrep ");
            }
            Ok(None) if started.elapsed() < Duration::from_secs(5) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                log_for_debugging(&format!("Ripgrep first use test error: {error}"));
                return false;
            }
        }
    }
}

/// Maps to CC `testRipgrepOnFirstUse`: one memoized, fire-and-forget probe.
pub fn test_ripgrep_on_first_use() {
    if RIPGREP_STATUS.lock().is_ok_and(|status| status.is_some())
        || RIPGREP_PROBE_STARTED.swap(true, Ordering::AcqRel)
    {
        return;
    }

    let spawn = std::thread::Builder::new()
        .name("cometix-ripgrep-probe".to_string())
        .spawn(|| {
            codesign_ripgrep_if_necessary();
            let config = get_ripgrep_config();
            let working = run_ripgrep_first_use_probe(&config);
            log_for_debugging(&format!(
                "Ripgrep first use test: {} (mode={}, path={})",
                if working { "PASSED" } else { "FAILED" },
                config.mode.as_str(),
                config.command
            ));
            if let Ok(mut status) = RIPGREP_STATUS.lock() {
                *status = Some(RipgrepAvailability {
                    working,
                    last_tested_ms: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|duration| duration.as_millis())
                        .unwrap_or(0),
                    config,
                });
            }
        });
    if let Err(error) = spawn {
        RIPGREP_PROBE_STARTED.store(false, Ordering::Release);
        log_for_debugging(&format!("Failed to start ripgrep first-use probe: {error}"));
    }
}

/// Maps to CC `ripGrepStream`.
pub fn rip_grep_stream<F>(
    args: &[String],
    target: &Path,
    abort: &AbortController,
    mut on_lines: F,
) -> Result<(), String>
where
    F: FnMut(&[String]),
{
    codesign_ripgrep_if_necessary();
    let config = get_ripgrep_config();
    let command = resolve_spawn_command(&config);
    let mut full_args = config.args.clone();
    full_args.extend(args.iter().cloned());
    full_args.push(target.display().to_string());

    let mut rg_command = Command::new(&command);
    // CC `ripGrepStream` spawn() inherits process.env; the carrier is its counterpart.
    crate::utils::subprocess_env::apply_process_env_std(&mut rg_command);
    let mut child = rg_command
        .args(&full_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("Error running ripgrep: {error}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "ripgrep missing stdout".to_string())?;
    let mut reader = BufReader::new(stdout);
    let mut remainder = String::new();
    let mut buf = [0u8; 64 * 1024];

    loop {
        if abort.is_aborted() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Ripgrep search aborted".to_string());
        }
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let chunk = String::from_utf8_lossy(&buf[..n]);
                let data = format!("{remainder}{chunk}");
                let mut parts = data.split('\n').map(str::to_string).collect::<Vec<_>>();
                remainder = parts.pop().unwrap_or_default();
                let lines = parts
                    .into_iter()
                    .map(|line| line.trim_end_matches('\r').to_string())
                    .collect::<Vec<_>>();
                if !lines.is_empty() {
                    on_lines(&lines);
                }
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Error reading ripgrep stdout: {error}"));
            }
        }
    }

    if abort.is_aborted() {
        let _ = child.kill();
        let _ = child.wait();
        return Err("Ripgrep search aborted".to_string());
    }

    let status = child
        .wait()
        .map_err(|error| format!("Error waiting for ripgrep: {error}"))?;
    if !remainder.is_empty() {
        on_lines(&[remainder.trim_end_matches('\r').to_string()]);
    }
    if status.success() || status.code() == Some(1) {
        Ok(())
    } else {
        Err(format!("ripgrep exited with code {:?}", status.code()))
    }
}

/// Maps to CC `ripGrepFileCount` — stream-count newlines from `rg --files`.
fn rip_grep_file_count(
    args: &[String],
    target: &Path,
    abort: &AbortController,
) -> Result<u64, String> {
    codesign_ripgrep_if_necessary();
    let config = get_ripgrep_config();
    let command = resolve_spawn_command(&config);
    let mut full_args = config.args.clone();
    full_args.extend(args.iter().cloned());
    full_args.push(target.display().to_string());

    let mut rg_command = Command::new(&command);
    // CC `ripGrepFileCount` spawn() inherits process.env; the carrier is its counterpart.
    crate::utils::subprocess_env::apply_process_env_std(&mut rg_command);
    let mut child = rg_command
        .args(&full_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("Error running ripgrep: {error}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "ripgrep missing stdout".to_string())?;
    let mut reader = BufReader::new(stdout);
    let mut buf = [0u8; 64 * 1024];
    let mut lines = 0u64;

    loop {
        if abort.is_aborted() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Ripgrep search aborted".to_string());
        }
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => lines += count_char_in_bytes(&buf[..n], b'\n') as u64,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Error reading ripgrep stdout: {error}"));
            }
        }
    }

    let status = child
        .wait()
        .map_err(|error| format!("Error waiting for ripgrep: {error}"))?;
    if status.success() || status.code() == Some(1) {
        Ok(lines)
    } else {
        Err(format!("rg --files exited {:?}", status.code()))
    }
}

/// Maps to CC `countFilesRoundedRg` (memoized on dirPath + ignorePatterns).
pub fn count_files_rounded_rg(
    dir_path: &Path,
    abort: &AbortController,
    ignore_patterns: &[String],
) -> Option<u64> {
    let cache_key = format!("{}|{}", dir_path.display(), ignore_patterns.join(","));
    if let Ok(guard) = COUNT_FILES_CACHE.lock() {
        if let Some(cache) = guard.as_ref() {
            if let Some(cached) = cache.get(&cache_key) {
                return *cached;
            }
        }
    }

    let home = crate::utils::node_os::homedir();
    if let (Ok(resolved_dir), Ok(resolved_home)) = (dir_path.canonicalize(), home.canonicalize()) {
        if resolved_dir == resolved_home {
            store_count_cache(&cache_key, None);
            return None;
        }
    }

    let mut args = vec!["--files".to_string(), "--hidden".to_string()];
    for pattern in ignore_patterns {
        args.push("--glob".to_string());
        args.push(format!("!{pattern}"));
    }

    let rounded = match rip_grep_file_count(&args, dir_path, abort) {
        Ok(0) => Some(0),
        Ok(count) => {
            let magnitude = (count as f64).log10().floor() as i32;
            let power = 10f64.powi(magnitude);
            Some(((count as f64 / power).round() * power) as u64)
        }
        Err(error) => {
            if !error.contains("aborted") {
                log_for_debugging(&format!("countFilesRoundedRg error: {error}"));
            }
            None
        }
    };
    store_count_cache(&cache_key, rounded);
    rounded
}

fn store_count_cache(key: &str, value: Option<u64>) {
    if let Ok(mut guard) = COUNT_FILES_CACHE.lock() {
        let cache = guard.get_or_insert_with(HashMap::new);
        cache.insert(key.to_string(), value);
    }
}

/// Maps to CC `ripGrep(args, target, abortSignal)`.
pub fn rip_grep(
    args: &[String],
    target: &Path,
    abort: &AbortController,
) -> Result<Vec<String>, String> {
    codesign_ripgrep_if_necessary();
    // Fire-and-forget first-use probe (CC `void testRipgrepOnFirstUse()`).
    test_ripgrep_on_first_use();

    match rip_grep_inner(args, target, abort, false) {
        Ok(lines) => Ok(lines),
        Err(error) if error.starts_with(EAGAIN_STDERR_RETRY_PREFIX) => {
            rip_grep_inner(args, target, abort, true)
        }
        Err(error) => Err(error),
    }
}

fn rip_grep_inner(
    args: &[String],
    target: &Path,
    abort: &AbortController,
    single_thread: bool,
) -> Result<Vec<String>, String> {
    rip_grep_inner_with_config(
        &get_ripgrep_config(),
        args,
        target,
        abort,
        single_thread,
        MAX_BUFFER_SIZE,
        None,
    )
}

fn rip_grep_inner_with_config(
    config: &RipgrepConfig,
    args: &[String],
    target: &Path,
    abort: &AbortController,
    single_thread: bool,
    max_buffer_size: usize,
    timeout_override: Option<Duration>,
) -> Result<Vec<String>, String> {
    let command = resolve_spawn_command(config);
    let mut full_args = config.args.clone();
    if single_thread {
        full_args.push("-j".to_string());
        full_args.push("1".to_string());
    }
    full_args.extend(args.iter().cloned());
    full_args.push(target.display().to_string());

    let mut rg_command = Command::new(&command);
    // CC `ripGrepRaw` execFile()/spawn() inherit process.env; the carrier is its counterpart.
    crate::utils::subprocess_env::apply_process_env_std(&mut rg_command);
    let mut child = match rg_command
        .args(&full_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            return Err(format!("Error running ripgrep: {error}"));
        }
        Err(error) => {
            // CC retries EAGAIN only when a child supplied that text on
            // stderr. Other noncritical spawn failures have no partial stdout
            // and resolve as an empty result.
            log_for_debugging(&format!("Noncritical ripgrep spawn error: {error}"));
            return Ok(Vec::new());
        }
    };

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "ripgrep missing stdout".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "ripgrep missing stderr".to_string())?;
    // Read both pipes concurrently with process polling. Waiting for the child
    // before draining can deadlock as soon as a large Glob result fills the OS
    // stdout pipe (CC execFile/spawn drains continuously).
    let buffer_overflow = Arc::new(AtomicBool::new(false));
    let stdout_overflow = Arc::clone(&buffer_overflow);
    let stderr_overflow = Arc::clone(&buffer_overflow);
    let stdout_reader =
        std::thread::spawn(move || drain_ripgrep_pipe(stdout, max_buffer_size, stdout_overflow));
    let stderr_reader =
        std::thread::spawn(move || drain_ripgrep_pipe(stderr, max_buffer_size, stderr_overflow));

    let timeout = timeout_override.unwrap_or_else(default_timeout);
    let start = Instant::now();
    loop {
        if abort.is_aborted() {
            let _ = child.kill();
            let _ = child.wait();
            let (stdout, _) = join_ripgrep_pipe(stdout_reader, "stdout")?;
            let (stderr, _) = join_ripgrep_pipe(stderr_reader, "stderr")?;
            if !single_thread && is_egain_error(&stderr) {
                return Err(format!("{EAGAIN_STDERR_RETRY_PREFIX}{stderr}"));
            }
            let mut lines = lines_from_stdout(&stdout);
            if !lines.is_empty() {
                // CC classifies ABORT_ERR with timeout/buffer termination and
                // drops the potentially torn final line.
                lines.pop();
            }
            if !lines.is_empty() {
                return Ok(lines);
            }
            return Err(format!(
                "Ripgrep search timed out after {} seconds. The search may have matched files but did not complete in time. Try searching a more specific path or pattern.",
                platform_default_timeout_seconds()
            ));
        }
        if config.mode != RipgrepMode::Embedded && buffer_overflow.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            let (stdout, _) = join_ripgrep_pipe(stdout_reader, "stdout")?;
            let (stderr, _) = join_ripgrep_pipe(stderr_reader, "stderr")?;
            if !single_thread && is_egain_error(&stderr) {
                return Err(format!("{EAGAIN_STDERR_RETRY_PREFIX}{stderr}"));
            }
            let mut lines = lines_from_stdout(&stdout);
            if !lines.is_empty() {
                // Node execFile reports ERR_CHILD_PROCESS_STDIO_MAXBUFFER;
                // CC drops its potentially torn final retained line.
                lines.pop();
            }
            return Ok(lines);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let (stdout, stdout_truncated) = join_ripgrep_pipe(stdout_reader, "stdout")?;
                let (stderr, _) = join_ripgrep_pipe(stderr_reader, "stderr")?;

                let buffer_was_truncated =
                    stdout_truncated || buffer_overflow.load(Ordering::Acquire);
                if status.success()
                    || (config.mode == RipgrepMode::Embedded && status.code() == Some(1))
                {
                    let mut lines = lines_from_stdout(&stdout);
                    if config.mode != RipgrepMode::Embedded
                        && buffer_was_truncated
                        && !lines.is_empty()
                    {
                        // Non-embedded CC uses execFile/maxBuffer and drops the
                        // retained tail. Embedded CC reports code 0/1 success
                        // and parses its capped output without this error path.
                        lines.pop();
                    }
                    return Ok(lines);
                }

                // In execFile mode code 1 arrives as an error and CC resolves
                // [] before inspecting stdout. Embedded mode handled it above.
                if status.code() == Some(1) {
                    return Ok(Vec::new());
                }

                if !single_thread && is_egain_error(&stderr) {
                    return Err(format!("{EAGAIN_STDERR_RETRY_PREFIX}{stderr}"));
                }

                let mut lines = lines_from_stdout(&stdout);
                let was_killed = status.code().is_none();
                if !lines.is_empty() && was_killed {
                    lines.pop();
                }
                if was_killed && lines.is_empty() {
                    return Err(format!(
                        "Ripgrep search timed out after {} seconds. The search may have matched files but did not complete in time. Try searching a more specific path or pattern.",
                        platform_default_timeout_seconds()
                    ));
                }
                // Maps to CC handleResult: noncritical nonzero exits (including
                // ripgrep usage code 2) resolve whatever complete stdout was
                // accumulated, even when that list is empty.
                return Ok(lines);
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    let (stdout, _) = join_ripgrep_pipe(stdout_reader, "stdout")?;
                    let (stderr, _) = join_ripgrep_pipe(stderr_reader, "stderr")?;
                    if !single_thread && is_egain_error(&stderr) {
                        return Err(format!("{EAGAIN_STDERR_RETRY_PREFIX}{stderr}"));
                    }
                    let mut lines = lines_from_stdout(&stdout);
                    if !lines.is_empty() {
                        // CC drops the potentially torn final line and returns
                        // completed partial results on timeout.
                        lines.pop();
                    }
                    if !lines.is_empty() {
                        return Ok(lines);
                    }
                    return Err(format!(
                        "Ripgrep search timed out after {} seconds. The search may have matched files but did not complete in time. Try searching a more specific path or pattern.",
                        platform_default_timeout_seconds()
                    ));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = join_ripgrep_pipe(stdout_reader, "stdout");
                let _ = join_ripgrep_pipe(stderr_reader, "stderr");
                return Err(format!("Error waiting for ripgrep: {error}"));
            }
        }
    }
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(windows)]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

#[cfg(not(any(unix, windows)))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

fn system_rg_path_in(path: &std::ffi::OsStr) -> Option<PathBuf> {
    for dir in std::env::split_paths(path) {
        #[cfg(windows)]
        {
            let path_ext = crate::utils::process_env::startup_snapshot()
                .var("PATHEXT")
                .unwrap_or(".COM;.EXE;.BAT;.CMD")
                .to_string();
            for extension in path_ext
                .split(';')
                .filter(|extension| !extension.is_empty())
            {
                let candidate = dir.join(format!("rg{extension}"));
                if is_executable_file(&candidate) {
                    return Some(candidate);
                }
            }
        }
        #[cfg(not(windows))]
        {
            let candidate = dir.join("rg");
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Resolve executable `rg` on PATH, mirroring CC's `which` / `where.exe` gate
/// (`findExecutable` → `whichSync`): Bun.which reads the startup PATH, as
/// `utils/which.rs` does.
pub fn system_rg_path() -> Option<PathBuf> {
    system_rg_path_in(crate::utils::process_env::startup_snapshot().var_os("PATH")?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_platform_directory_uses_official_node_names() {
        let expected_arch = match std::env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            other => other,
        };
        let expected_os = match std::env::consts::OS {
            "macos" => "darwin",
            "windows" => "win32",
            other => other,
        };
        assert_eq!(
            vendor_ripgrep_platform_dir(),
            format!("{expected_arch}-{expected_os}")
        );
    }

    #[cfg(unix)]
    #[test]
    fn system_lookup_skips_non_executable_path_entries_like_which() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "cometix-rg-which-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let first = root.join("first");
        let second = root.join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let non_executable = first.join("rg");
        let executable = second.join("rg");
        std::fs::write(&non_executable, "not executable").unwrap();
        std::fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&non_executable, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::join_paths([&first, &second]).unwrap();
        assert_eq!(system_rg_path_in(&path), Some(executable));
        let _ = std::fs::remove_dir_all(root);
    }

    /// The packaged binary is an external asset, not something compiled in, so
    /// the lookup either finds a real executable or reports nothing and lets
    /// the caller fall back to a host `rg`.
    #[test]
    fn packaged_ripgrep_lookup_returns_a_runnable_binary_or_nothing() {
        let Some(binary) = vendor_rg_path() else {
            // No checked-out vendor directory (an installed build, or a source
            // tree without one). The config path falls back to system `rg`.
            return;
        };
        assert!(binary.is_file(), "the lookup only reports real files");
        let output = Command::new(&binary)
            .arg("--version")
            .output()
            .expect("run packaged rg");
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).starts_with("ripgrep "));
    }

    #[test]
    fn all_official_standalone_ripgrep_targets_are_packaged() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("vendor/ripgrep");
        if !root.is_dir() {
            // The packaged binaries are an external asset, not a build input:
            // a checkout without them is valid and falls back to a host `rg`.
            // Where they ARE checked out, every official target must be there.
            return;
        }
        for (directory, binary) in [
            ("x64-darwin", "rg"),
            ("arm64-darwin", "rg"),
            ("x64-linux", "rg"),
            ("arm64-linux", "rg"),
            ("x64-win32", "rg.exe"),
            ("arm64-win32", "rg.exe"),
        ] {
            let path = root.join(directory).join(binary);
            assert!(path.is_file(), "missing packaged {}", path.display());
            assert!(std::fs::metadata(&path).unwrap().len() > 1_000_000);
        }
        assert!(root.join("COPYING").is_file());
    }

    #[test]
    fn glob_timeout_env_uses_javascript_parse_int_prefix_semantics() {
        for (input, expected) in [
            ("", 0),
            ("  15seconds", 15),
            ("+7", 7),
            ("-3", 0),
            ("0", 0),
            ("garbage", 0),
        ] {
            assert_eq!(parse_timeout_seconds(input), expected, "input={input:?}");
        }
    }

    #[test]
    fn rip_grep_finds_matches_like_official() {
        let dir =
            std::env::temp_dir().join(format!("cometix-rg-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "hello ripgrep\n").unwrap();
        let lines = rip_grep(
            &["--hidden".into(), "-l".into(), "ripgrep".into()],
            &dir,
            &AbortController::default(),
        )
        .expect("rg runs");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(lines.iter().any(|line| line.ends_with("a.txt")));
        for _ in 0..100 {
            if get_ripgrep_status().working.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(get_ripgrep_status().working.is_some());
    }

    #[test]
    fn rip_grep_drains_large_stdout_while_child_is_running() {
        let dir = std::env::temp_dir().join(format!(
            "cometix-rg-large-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for index in 0..4_000 {
            std::fs::write(
                dir.join(format!("file-{index:04}-with-a-long-enough-name.txt")),
                "x",
            )
            .unwrap();
        }
        let lines = rip_grep(
            &["--files".into(), "--sort=path".into()],
            &dir,
            &AbortController::default(),
        )
        .expect("large rg stdout completes without a pipe deadlock");
        assert_eq!(lines.len(), 4_000);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn exit_one_discards_stdout_only_in_execfile_modes() {
        let shell = if Path::new("/bin/sh").is_file() {
            "/bin/sh"
        } else {
            "/usr/bin/sh"
        };
        let args = vec![
            "-c".to_string(),
            "printf 'unexpected\\n'; exit 1".to_string(),
            "sh".to_string(),
        ];
        let builtin = RipgrepConfig {
            mode: RipgrepMode::Builtin,
            command: shell.to_string(),
            args: args.clone(),
            argv0: None,
        };
        assert!(
            rip_grep_inner_with_config(
                &builtin,
                &[],
                Path::new("ignored"),
                &AbortController::default(),
                false,
                MAX_BUFFER_SIZE,
                None,
            )
            .unwrap()
            .is_empty()
        );

        let embedded = RipgrepConfig {
            mode: RipgrepMode::Embedded,
            command: shell.to_string(),
            args,
            argv0: Some("rg".to_string()),
        };
        assert_eq!(
            rip_grep_inner_with_config(
                &embedded,
                &[],
                Path::new("ignored"),
                &AbortController::default(),
                false,
                MAX_BUFFER_SIZE,
                None,
            )
            .unwrap(),
            vec!["unexpected".to_string()]
        );
    }

    #[cfg(unix)]
    #[test]
    fn timeout_branch_preserves_child_stderr_eagain_retry_signal() {
        let shell = if Path::new("/bin/sh").is_file() {
            "/bin/sh"
        } else {
            "/usr/bin/sh"
        };
        let config = RipgrepConfig {
            mode: RipgrepMode::Builtin,
            command: shell.to_string(),
            args: vec![
                "-c".to_string(),
                "printf 'Resource temporarily unavailable\\n' >&2; exec /bin/sleep 10".to_string(),
                "sh".to_string(),
            ],
            argv0: None,
        };
        let error = rip_grep_inner_with_config(
            &config,
            &[],
            Path::new("ignored"),
            &AbortController::default(),
            false,
            MAX_BUFFER_SIZE,
            Some(Duration::from_millis(50)),
        )
        .unwrap_err();
        assert!(error.starts_with(EAGAIN_STDERR_RETRY_PREFIX));
    }

    #[cfg(unix)]
    #[test]
    fn nonembedded_mode_kills_at_execfile_buffer_cap_and_drops_tail() {
        let yes = ["/usr/bin/yes", "/bin/yes"]
            .into_iter()
            .find(|path| Path::new(path).is_file())
            .expect("yes executable");
        let config = RipgrepConfig {
            mode: RipgrepMode::Builtin,
            command: yes.to_string(),
            args: Vec::new(),
            argv0: None,
        };
        let started = Instant::now();
        let lines = rip_grep_inner_with_config(
            &config,
            &[],
            Path::new("complete-line"),
            &AbortController::default(),
            false,
            64 * 1024,
            None,
        )
        .expect("buffer overflow remains a noncritical partial result");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(!lines.is_empty());
        assert!(lines.iter().all(|line| line == "complete-line"));
        assert!(lines.len() * "complete-line\n".len() < 64 * 1024);
    }

    #[test]
    fn rip_grep_stream_emits_lines() {
        let dir = std::env::temp_dir().join(format!(
            "cometix-rg-stream-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("b.txt"), "stream-needle\n").unwrap();
        let mut seen = Vec::new();
        rip_grep_stream(
            &["--hidden".into(), "-l".into(), "stream-needle".into()],
            &dir,
            &AbortController::default(),
            |lines| seen.extend(lines.iter().cloned()),
        )
        .expect("stream runs");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(seen.iter().any(|line| line.ends_with("b.txt")));
    }

    #[test]
    fn count_files_rounded_rg_rounds_like_official() {
        let dir = std::env::temp_dir().join(format!(
            "cometix-rg-count-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..8 {
            std::fs::write(dir.join(format!("f{i}.txt")), "x\n").unwrap();
        }
        let count = count_files_rounded_rg(&dir, &AbortController::default(), &[]).expect("count");
        // CC: power = 10^floor(log10(n)); 8 → 10^0=1 → round(8)*1 = 8
        assert_eq!(count, 8);
        // Memoized second call returns the same cached value without re-walk.
        assert_eq!(
            count_files_rounded_rg(&dir, &AbortController::default(), &[]),
            Some(8)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
