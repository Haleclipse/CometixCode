//! Debug/profiling switches + `logForDebugging`.
//!
//! Maps to: CC `utils/debug.ts`. Category filtering lives in
//! [`crate::utils::debug_filter`] (CC `utils/debugFilter.ts`).
//!
//! Developer defaults (aligned with CC):
//! - `--debug` / `-d` → write to `~/.claude/debug/{session}.log` (TUI-safe;
//!   Cometix uses `.log`; CC uses `.txt`)
//! - `--debug-to-stderr` / `-d2e` → stderr instead of file
//! - `--debug-file <path>` → explicit file (implies debug mode)
//! - `--debug=api,hooks` / `-d api,hooks` → category filter
//!
//! Cometix-only render profilers (CC has no equivalent; see
//! [`ProfileSelection`]): `--debug=frame,component,query-pump` or
//! `--debug=perf`, the argv flags `--debug-*-profile`, or the environment
//! variable `COMETIX_DEBUG_PROFILES` with the same list syntax.
//! `COMETIX_FRAME_TIMING_LOG` / `COMETIX_FRAME_TIMING_SAMPLE_EVERY` are the
//! twins of CC's `CLAUDE_CODE_FRAME_TIMING_LOG` / `_SAMPLE_EVERY` bench JSONL.

use crate::utils::debug_filter::{DebugFilter, parse_debug_filter, should_show_debug_message};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Maps to: CC `utils/debug.ts` `DebugLogLevel` + `LEVEL_ORDER`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DebugLogLevel {
    Verbose,
    Debug,
    Info,
    Warn,
    Error,
}

impl DebugLogLevel {
    fn label(self) -> &'static str {
        match self {
            Self::Verbose => "VERBOSE",
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }

    fn from_env_name(raw: &str) -> Option<Self> {
        match raw.to_lowercase().as_str() {
            "verbose" => Some(Self::Verbose),
            "debug" => Some(Self::Debug),
            "info" => Some(Self::Info),
            "warn" => Some(Self::Warn),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

/// Maps to: CC `utils/debug.ts#getMinDebugLogLevel`.
pub fn get_min_debug_log_level() -> DebugLogLevel {
    std::env::var("CLAUDE_CODE_DEBUG_LOG_LEVEL")
        .ok()
        .as_deref()
        .and_then(DebugLogLevel::from_env_name)
        .unwrap_or(DebugLogLevel::Debug)
}

/// Maps to: CC `utils/debug.ts#getDebugLogPath`.
pub fn get_debug_log_path() -> PathBuf {
    if let Some(path) = config().debug_file.clone() {
        return path;
    }
    if let Ok(dir) = std::env::var("CLAUDE_CODE_DEBUG_LOGS_DIR") {
        return PathBuf::from(dir)
            .join(format!("{}.log", crate::bootstrap::state::get_session_id()));
    }
    crate::utils::config::get_config_home()
        .join("debug")
        .join(format!("{}.log", crate::bootstrap::state::get_session_id()))
}

/// Maps to: CC `utils/debug.ts#logForDebugging`.
pub fn log_for_debugging(message: &str) {
    log_for_debugging_with_level(message, DebugLogLevel::Debug);
}

/// Maps to: CC `utils/debug.ts:258-268` `logAntError`.
/// `error_stack` projects `error instanceof Error && error.stack`: callers pass
/// `None` for non-Error values or absent stacks. A Rust error/backtrace is the
/// native stack carrier; it must not be presented as a JavaScript stack.
pub fn log_ant_error(context: &str, error_stack: Option<&str>) {
    if !crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::TelemetryPayloads,
    ) {
        return;
    }
    if let Some(stack) = error_stack.filter(|stack| !stack.is_empty()) {
        log_for_debugging_with_level(
            &format!("[ANT-ONLY] {context} stack trace:\n{stack}"),
            DebugLogLevel::Error,
        );
    }
}

pub fn log_for_debugging_with_level(message: &str, level: DebugLogLevel) {
    if level < get_min_debug_log_level() {
        return;
    }
    if !should_log_debug_message(message) {
        return;
    }

    // CC: multiline messages break the jsonl output format, so make any
    // multiline messages JSON.
    let message = if message.contains('\n') {
        serde_json::to_string(message).unwrap_or_else(|_| message.to_string())
    } else {
        message.to_string()
    };
    let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ");
    let output = format!("{timestamp} [{}] {}\n", level.label(), message.trim());

    if is_debug_to_stderr() {
        eprint!("{output}");
        return;
    }

    let path = get_debug_log_path();
    append_debug_log(&path, &output);
    update_latest_debug_log_symlink(&path);
}

/// Maps to: CC `utils/debug.ts:104-125` `shouldLogDebugMessage`.
fn should_log_debug_message(message: &str) -> bool {
    // cfg(test): skip file spam unless explicitly stderr (CC NODE_ENV=test).
    if cfg!(test) && !is_debug_to_stderr() {
        return false;
    }
    if !crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::TelemetryPayloads,
    ) && !is_debug_mode()
    {
        return false;
    }
    should_show_debug_message(message, config().filter.as_ref())
}

fn append_debug_log(path: &Path, output: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(output.as_bytes());
    }
}

/// Maps to: CC `updateLatestDebugLogSymlink` (best-effort).
fn update_latest_debug_log_symlink(debug_log_path: &Path) {
    let Some(dir) = debug_log_path.parent() else {
        return;
    };
    // Only maintain `latest` for the default debug directory layout.
    if config().debug_file.is_some() {
        return;
    }
    let latest = dir.join("latest");
    let _ = std::fs::remove_file(&latest);
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let _ = symlink(debug_log_path, &latest);
    }
}

/// Which render-side profilers are on. Cometix-only (CC has no equivalent of
/// these probes); each costs per frame, so all are off by default.
///
/// One comma-separated list syntax selects them in three places: the
/// `--debug=` filter (profiler names sit beside the log categories), the argv
/// flags `--debug-frame-profile` / `--debug-component-profile` /
/// `--debug-query-pump-profile` / `--debug-perf`, and `COMETIX_DEBUG_PROFILES`
/// for launching under a profiler without touching argv (bench scripts).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProfileSelection {
    /// Per-frame phase timings (`frame_profile_enabled`).
    pub frame: bool,
    /// Per-component update timings (`component_profile_enabled`).
    pub component: bool,
    /// Query pump iteration timings (`query_pump_profile_enabled`).
    pub query_pump: bool,
}

impl ProfileSelection {
    pub const ALL: Self = Self {
        frame: true,
        component: true,
        query_pump: true,
    };

    /// Parse a profiler list. `frame`, `component` and `query-pump` each
    /// select one profiler; `all`, `perf`, `profile`, `profiles`, or a bare
    /// truthy value (`1`, `true`) select every profiler. Anything else is
    /// ignored: the filter's `!name` exclusion spelling and the log categories
    /// that share the `--debug=` list never select a profiler.
    fn parse(list: Option<&str>) -> Self {
        let Some(list) = list else {
            return Self::default();
        };
        if crate::utils::env_utils::is_env_truthy(Some(list)) {
            return Self::ALL;
        }
        let mut selection = Self::default();
        for token in list.split(',').map(str::trim) {
            match token.to_ascii_lowercase().as_str() {
                "frame" => selection.frame = true,
                "component" => selection.component = true,
                "query-pump" | "query_pump" => selection.query_pump = true,
                "all" | "perf" | "profile" | "profiles" => selection = Self::ALL,
                _ => {}
            }
        }
        selection
    }

    /// `COMETIX_DEBUG_PROFILES`: the same list, from the environment.
    fn from_env() -> Self {
        Self::parse(std::env::var("COMETIX_DEBUG_PROFILES").ok().as_deref())
    }

    fn or(self, other: Self) -> Self {
        Self {
            frame: self.frame || other.frame,
            component: self.component || other.component,
            query_pump: self.query_pump || other.query_pump,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DebugConfig {
    pub debug: bool,
    pub debug_to_stderr: bool,
    pub debug_file: Option<PathBuf>,
    pub filter: Option<DebugFilter>,
    pub profiles: ProfileSelection,
}

static DEBUG_CONFIG: OnceLock<DebugConfig> = OnceLock::new();

/// Initialize from structured CLI fields (no dependency on `cli` module).
pub fn init_from_parts(
    debug: bool,
    debug_to_stderr: bool,
    debug_file: Option<PathBuf>,
    debug_filter: Option<&str>,
) {
    let _ = DEBUG_CONFIG.set(DebugConfig::from_parts(
        debug,
        debug_to_stderr,
        debug_file,
        debug_filter,
    ));
}

/// Initialize from raw argv (legacy / early paths). Prefer
/// [`init_from_parts`] after structured parse.
pub fn init_from_argv(argv: &[String]) {
    let _ = DEBUG_CONFIG.set(DebugConfig::from_argv(argv));
}

/// One-line stderr tip + first logfile entry so `--debug` is immediately useful.
pub fn announce_debug_startup_if_enabled() {
    if !is_debug_mode() {
        return;
    }
    let sink = if is_debug_to_stderr() {
        "stderr".to_string()
    } else {
        get_debug_log_path().display().to_string()
    };
    // Always print destination once before the TUI owns the terminal.
    eprintln!("[cometix] debug logging → {sink}");
    log_for_debugging(&format!("startup: debug mode enabled → {sink}"));
    if let Some(filter) = config().filter.as_ref() {
        log_for_debugging(&format!("startup: debug filter={filter:?}"));
    }
}

pub fn is_debug_mode() -> bool {
    config().debug
}

/// Whether the active debug filter would include API-category messages.
///
/// Used by `services/api/api_trace` so `--debug=api` (or unfiltered `--debug`)
/// implies at least summary-level JSONL tracing.
pub fn debug_filter_matches_api() -> bool {
    if !is_debug_mode() {
        return false;
    }
    match config().filter.as_ref() {
        None => true,
        Some(filter) if filter.is_exclusive => !filter.exclude.iter().any(|cat| {
            cat == "api" || cat.starts_with("api:") || cat == "api:trace" || cat == "api:request"
        }),
        Some(filter) => filter.include.iter().any(|cat| {
            cat == "api"
                || cat == "all"
                || cat.starts_with("api:")
                || cat == "api:trace"
                || cat == "api:request"
        }),
    }
}

pub fn is_debug_to_stderr() -> bool {
    config().debug_to_stderr
}

pub fn frame_profile_enabled() -> bool {
    config().profiles.frame
}

/// Maps to: CC `interactiveHelpers.tsx:427` `CLAUDE_CODE_FRAME_TIMING_LOG` —
/// bench-only per-frame JSONL for offline analysis. The Rust twin uses the
/// same record shape so one analysis script can consume both sides.
pub fn frame_timing_log_path() -> Option<std::path::PathBuf> {
    std::env::var_os("COMETIX_FRAME_TIMING_LOG")
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
}

/// Maps to: CC `main.tsx` `CLAUDE_CODE_FRAME_TIMING_SAMPLE_EVERY` — record the
/// heavyweight rss/cpu samples only every Nth frame (default 1: every frame).
/// Returns true when this frame should carry the samples.
pub fn frame_timing_sample_tick() -> bool {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    static EVERY: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let every = *EVERY.get_or_init(|| {
        std::env::var("COMETIX_FRAME_TIMING_SAMPLE_EVERY")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|&value| value >= 1)
            .unwrap_or(1)
    });
    COUNTER.fetch_add(1, Ordering::Relaxed) % every == 0
}

/// Cumulative process CPU time in microseconds, the Rust twin of Node
/// `process.cpuUsage()` (which CC's frame-timing log records per frame; the
/// bench side computes deltas).
pub fn process_cpu_usage_micros() -> (u64, u64) {
    #[cfg(unix)]
    {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: RUSAGE_SELF with a zeroed out-param is the documented usage.
        let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
        if result != 0 {
            return (0, 0);
        }
        // SAFETY: getrusage returned success, so the struct is initialized.
        let usage = unsafe { usage.assume_init() };
        let to_micros =
            |time: libc::timeval| time.tv_sec as u64 * 1_000_000 + time.tv_usec as u64;
        (to_micros(usage.ru_utime), to_micros(usage.ru_stime))
    }
    #[cfg(not(unix))]
    {
        (0, 0)
    }
}

pub fn component_profile_enabled() -> bool {
    config().profiles.component
}

pub fn query_pump_profile_enabled() -> bool {
    config().profiles.query_pump
}

fn config() -> &'static DebugConfig {
    DEBUG_CONFIG.get_or_init(|| {
        let argv = std::env::args().collect::<Vec<_>>();
        DebugConfig::from_argv(&argv)
    })
}

impl DebugConfig {
    pub fn from_parts(
        debug_flag: bool,
        debug_to_stderr: bool,
        debug_file: Option<PathBuf>,
        debug_filter: Option<&str>,
    ) -> Self {
        let filter = parse_debug_filter(debug_filter);
        let debug = debug_flag
            || debug_to_stderr
            || debug_file.is_some()
            || debug_filter.is_some()
            || crate::utils::env_utils::is_env_truthy(std::env::var("DEBUG").ok().as_deref())
            || crate::utils::env_utils::is_env_truthy(std::env::var("DEBUG_SDK").ok().as_deref())
            || crate::utils::env_utils::is_env_truthy(
                std::env::var("CLAUDE_CODE_DEBUG").ok().as_deref(),
            );

        let profiles = ProfileSelection::parse(debug_filter).or(ProfileSelection::from_env());

        Self {
            debug,
            // CC: only --debug-to-stderr / -d2e write to stderr; bare --debug
            // goes to the session debug file so the TUI stays clean.
            debug_to_stderr,
            debug_file,
            filter,
            profiles,
        }
    }

    pub fn from_argv(argv: &[String]) -> Self {
        let filter_raw = debug_filter_from_argv(argv);
        let debug_file = parse_flag_value(argv, "--debug-file").map(PathBuf::from);
        let debug_to_stderr =
            has_exact_flag(argv, "--debug-to-stderr") || has_exact_flag(argv, "-d2e");
        let debug_flag = has_exact_flag(argv, "--debug")
            || has_exact_flag(argv, "-d")
            || filter_raw.is_some()
            || argv.iter().any(|a| a.starts_with("--debug="))
            || has_exact_flag(argv, "--debug-profile")
            || has_exact_flag(argv, "--debug-profiles")
            || has_exact_flag(argv, "--debug-perf")
            || has_exact_flag(argv, "--debug-frame-profile")
            || has_exact_flag(argv, "--debug-component-profile")
            || has_exact_flag(argv, "--debug-query-pump-profile");

        let mut cfg = Self::from_parts(debug_flag, debug_to_stderr, debug_file, filter_raw);
        // Argv-only spellings of the same selection; the filter and the
        // environment were already folded in by `from_parts`.
        let all = has_exact_flag(argv, "--debug-profile")
            || has_exact_flag(argv, "--debug-profiles")
            || has_exact_flag(argv, "--debug-perf");
        cfg.profiles = cfg.profiles.or(ProfileSelection {
            frame: all || has_exact_flag(argv, "--debug-frame-profile"),
            component: all || has_exact_flag(argv, "--debug-component-profile"),
            query_pump: all || has_exact_flag(argv, "--debug-query-pump-profile"),
        });
        cfg
    }
}

fn debug_filter_from_argv(argv: &[String]) -> Option<&str> {
    for (index, arg) in argv.iter().enumerate() {
        if let Some(value) = arg.strip_prefix("--debug=") {
            return (!value.is_empty()).then_some(value);
        }
        if arg == "--debug" || arg == "-d" {
            if let Some(next) = argv.get(index + 1) {
                if !next.starts_with('-') {
                    // Commander `-d [filter]`: next non-flag token is filter.
                    return Some(next.as_str());
                }
            }
        }
    }
    None
}

fn has_exact_flag(argv: &[String], flag: &str) -> bool {
    argv.iter().any(|arg| arg == flag)
}

fn parse_flag_value<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
    let equals_prefix = format!("{flag}=");
    for (index, arg) in argv.iter().enumerate() {
        if let Some(value) = arg.strip_prefix(&equals_prefix) {
            return Some(value);
        }
        if arg == flag {
            return argv.get(index + 1).map(String::as_str);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_ant_error_matches_official_audience_stack_and_error_level() {
        const CHILD: &str = "COMETIX_TEST_LOG_ANT_ERROR_CHILD";
        if std::env::var_os(CHILD).is_some() {
            init_from_parts(false, true, None, None);
            log_ant_error("absent-stack", None);
            log_ant_error("empty-stack", Some(""));
            log_ant_error(
                "attachment",
                Some("Error: unknown attachment\n  native frame"),
            );
            return;
        }

        // Separate process keeps the production OnceLock and stderr sink real
        // without changing other tests' debug configuration.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "utils::debug::tests::log_ant_error_matches_official_audience_stack_and_error_level",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env_remove("CLAUDE_CODE_DEBUG_LOG_LEVEL")
            .output()
            .unwrap();
        assert!(output.status.success());
        let stderr = String::from_utf8(output.stderr).unwrap();
        if cfg!(feature = "anthropic_internal") {
            assert!(stderr.contains("[ERROR]"), "{stderr}");
            assert!(stderr.contains("[ANT-ONLY] attachment stack trace:\\nError: unknown attachment\\n  native frame"), "{stderr}");
        } else {
            assert!(stderr.is_empty(), "{stderr}");
        }
        assert!(!stderr.contains("absent-stack"));
        assert!(!stderr.contains("empty-stack"));
    }

    fn argv(args: &[&str]) -> Vec<String> {
        std::iter::once("cometix".to_string())
            .chain(args.iter().map(|arg| arg.to_string()))
            .collect()
    }

    #[test]
    fn debug_flag_enables_debug_mode_but_not_stderr() {
        let config = DebugConfig::from_argv(&argv(&["--debug"]));
        assert!(config.debug);
        assert!(!config.debug_to_stderr);
        assert_eq!(config.profiles, ProfileSelection::default());
        assert!(config.debug_file.is_none());
    }

    #[test]
    fn debug_to_stderr_flag() {
        let config = DebugConfig::from_argv(&argv(&["-d2e"]));
        assert!(config.debug);
        assert!(config.debug_to_stderr);
    }

    #[test]
    fn debug_file_enables_debug_and_sets_path() {
        let config = DebugConfig::from_argv(&argv(&["--debug-file", "/tmp/cometix-debug.log"]));
        assert!(config.debug);
        assert_eq!(
            config.debug_file.as_deref(),
            Some(Path::new("/tmp/cometix-debug.log"))
        );
    }

    #[test]
    fn debug_equals_filter_parses() {
        let config = DebugConfig::from_argv(&argv(&["--debug=api,hooks"]));
        assert!(config.debug);
        let filter = config.filter.expect("filter");
        assert_eq!(filter.include, vec!["api", "hooks"]);
    }

    #[test]
    fn debug_profile_enables_perf_probes() {
        let config = DebugConfig::from_argv(&argv(&["--debug=perf"]));
        assert!(config.debug);
        assert_eq!(config.profiles, ProfileSelection::ALL);
    }

    #[test]
    fn filter_names_select_single_profilers_beside_log_categories() {
        let config = DebugConfig::from_argv(&argv(&["--debug=api,frame"]));
        assert_eq!(
            config.profiles,
            ProfileSelection {
                frame: true,
                component: false,
                query_pump: false,
            }
        );
        // The exclusion spelling never selects a profiler.
        let config = DebugConfig::from_argv(&argv(&["--debug=!perf"]));
        assert_eq!(config.profiles, ProfileSelection::default());
    }

    #[test]
    fn argv_profile_flags_add_to_the_filter_selection() {
        let config = DebugConfig::from_argv(&argv(&["--debug-query-pump-profile"]));
        assert!(config.debug);
        assert_eq!(
            config.profiles,
            ProfileSelection {
                frame: false,
                component: false,
                query_pump: true,
            }
        );
        let config = DebugConfig::from_argv(&argv(&["--debug=component", "--debug-perf"]));
        assert_eq!(config.profiles, ProfileSelection::ALL);
    }

    #[test]
    fn profile_list_syntax() {
        let one = |frame, component, query_pump| ProfileSelection {
            frame,
            component,
            query_pump,
        };
        assert_eq!(ProfileSelection::parse(None), ProfileSelection::default());
        assert_eq!(ProfileSelection::parse(Some("")), ProfileSelection::default());
        assert_eq!(ProfileSelection::parse(Some("1")), ProfileSelection::ALL);
        assert_eq!(ProfileSelection::parse(Some("true")), ProfileSelection::ALL);
        assert_eq!(
            ProfileSelection::parse(Some(" Frame , query-pump ")),
            one(true, false, true)
        );
        assert_eq!(
            ProfileSelection::parse(Some("component,all")),
            ProfileSelection::ALL
        );
        // Retired spellings and unknown tokens select nothing.
        assert_eq!(
            ProfileSelection::parse(Some("frame-profile,COMETIX_FRAME_PROFILE")),
            ProfileSelection::default()
        );
    }

    #[test]
    fn from_parts_matches_file_default() {
        let config = DebugConfig::from_parts(true, false, None, Some("mcp"));
        assert!(config.debug);
        assert!(!config.debug_to_stderr);
        assert_eq!(config.filter.unwrap().include, vec!["mcp"]);
    }
}
