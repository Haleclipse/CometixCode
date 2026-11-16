//! @nyan: TUI footer metrics (docs/nyan/tui.md §3.1, §5).
//!
//! Collection only. Later slices render these fields; nothing here paints UI.
//!
//! `TokenUsage` (`types/message.rs`) and `ir::Usage` are the same three-way
//! split. `services/api/ir_bridge.rs::token_usage` copies
//! `input_tokens` → `input_tokens` (already the uncached remainder),
//! `cache_write_tokens` → `cache_creation_input_tokens`,
//! `cache_read_tokens` → `cache_read_input_tokens`. `cache_write_1h_tokens`
//! and `reasoning_tokens` are subsets already inside `cache_write_tokens` and
//! `output_tokens`, so they are not added again. `Usage::cache_hit_rate` is
//! therefore the rate a message-level `TokenUsage` would produce.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::store::{AppStore, UpdateDecision};
use crate::services::provider::ir::{Delta, Usage};
use crate::utils::forked_agent::{is_agent_api_source, is_cache_sharing_api_source};

/// Ignore decode windows shorter than this. A few tokens over a few
/// milliseconds spike the displayed rate (docs/nyan/tui.md §5.1).
pub const MIN_DECODE_MS: u64 = 200;

/// Output tokens and decode time for one user turn. Rate is the ratio of the
/// sums, not the mean of per-request rates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TpsTurn {
    pub output_tokens: u64,
    pub decode_ms: u64,
}

impl TpsTurn {
    pub fn is_empty(self) -> bool {
        self.decode_ms == 0
    }

    pub fn tokens_per_second(self) -> Option<f64> {
        if self.decode_ms == 0 {
            None
        } else {
            Some(self.output_tokens as f64 * 1000.0 / self.decode_ms as f64)
        }
    }

    fn add(&mut self, output_tokens: u64, decode_ms: u64) {
        self.output_tokens = self.output_tokens.saturating_add(output_tokens);
        self.decode_ms = self.decode_ms.saturating_add(decode_ms);
    }
}

/// Latest main-thread cache sample and the session display gate.
#[derive(Clone, Debug, PartialEq)]
pub struct CacheMetrics {
    /// `Usage::cache_hit_rate` of the latest counted main-thread request.
    /// `None` until that request exists.
    pub hit_rate: Option<f64>,
    /// A counted main-thread request has reported `cache_read > 0` or
    /// `cache_write > 0`. Providers that never send cache fields stay hidden.
    pub session_reported_cache: bool,
    /// `Usage::detect_cache_miss` returned `Some` on a request that was not
    /// exempt. Stays set until a later non-exempt request is healthy.
    pub miss_warning: bool,
}

impl Default for CacheMetrics {
    fn default() -> Self {
        Self {
            hit_rate: None,
            session_reported_cache: false,
            miss_warning: false,
        }
    }
}

/// Branch, sha and linked-worktree flag. Filled off the render path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitSnapshot {
    /// `false` until the first probe finishes.
    pub probed: bool,
    pub in_repo: bool,
    /// Branch name. `None` when detached or outside a repo.
    pub branch: Option<String>,
    /// Short sha. Set whenever `in_repo`, including while on a branch.
    pub short_sha: Option<String>,
    pub detached: bool,
    /// `git-dir` and `git-common-dir` differ.
    pub linked_worktree: bool,
}

/// Everything slice C reads from AppState for the info line.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FooterMetrics {
    /// Samples since the latest user submit. Empty after `start_user_turn`
    /// until the first accepted request of that turn.
    pub current_turn: TpsTurn,
    /// Last turn that had samples. Shown while `current_turn` is empty.
    pub previous_turn: TpsTurn,
    /// A user turn's query is running. Slice C uses this with `current_turn`
    /// for the "generating" color; the spinner itself is not in AppState.
    pub turn_open: bool,
    pub cache: CacheMetrics,
    pub git: GitSnapshot,
    /// Directory the next git probe uses when the caller does not pass one.
    pub git_cwd: Option<PathBuf>,
    /// Bookkeeping for `detect_cache_miss`. Not rendered.
    pub(crate) seen_main_request: bool,
    pub(crate) suppress_cache_miss: bool,
    pub(crate) last_model: Option<String>,
    pub(crate) last_prompt_tokens: u64,
}

impl FooterMetrics {
    /// The turn to display: this turn once it has a sample, otherwise the
    /// previous one. `None` when the session has no sample yet.
    pub fn displayed_turn(&self) -> Option<TpsTurn> {
        if !self.current_turn.is_empty() {
            Some(self.current_turn)
        } else if !self.previous_turn.is_empty() {
            Some(self.previous_turn)
        } else {
            None
        }
    }

    /// Rotate samples and mark the turn generating. A second call in the same
    /// turn (current already empty, flag already set) is a no-op.
    pub fn start_turn(&mut self) {
        if !self.current_turn.is_empty() {
            self.previous_turn = self.current_turn;
            self.current_turn = TpsTurn::default();
        }
        self.turn_open = true;
    }

    pub fn finish_turn(&mut self) {
        self.turn_open = false;
    }

    fn accept_tps(&mut self, output_tokens: u64, decode_ms: u64) {
        self.current_turn.add(output_tokens, decode_ms);
    }

    /// Record one main-thread request's cache sample.
    ///
    /// Exempt from the miss warning: the session's first counted request, the
    /// first request after compaction, and the first request after the model
    /// string changes. Prices are passed through to `detect_cache_miss`; the
    /// caller passes `0.0` when the price is unknown.
    pub fn observe_cache(
        &mut self,
        model: &str,
        usage: &Usage,
        price_per_million_input: f64,
        price_per_million_cache_read: f64,
    ) {
        let exempt = !self.seen_main_request
            || self.suppress_cache_miss
            || self.last_model.as_deref() != Some(model);
        let miss = if exempt {
            None
        } else {
            usage.detect_cache_miss(
                self.last_prompt_tokens,
                price_per_million_input,
                price_per_million_cache_read,
            )
        };

        self.cache.hit_rate = Some(usage.cache_hit_rate());
        if usage.cache_read_tokens > 0 || usage.cache_write_tokens > 0 {
            self.cache.session_reported_cache = true;
        }
        if !exempt {
            self.cache.miss_warning = miss.is_some();
        }

        self.seen_main_request = true;
        self.suppress_cache_miss = false;
        self.last_model = Some(model.to_string());
        self.last_prompt_tokens = usage.prompt_tokens();
    }

    pub fn note_compaction(&mut self) {
        self.suppress_cache_miss = true;
    }
}

/// Main-thread requests only. `RetryQuerySource::ReplMainThread` is the exact
/// string `repl_main_thread`. Sub-agents and cache-sharing forks are rejected
/// even though their sources are already different strings.
pub fn counts_for_footer(query_source: &str) -> bool {
    query_source == "repl_main_thread"
        && !is_cache_sharing_api_source(query_source)
        && !is_agent_api_source(query_source)
}

/// Text, thinking, or tool-input deltas. Empty fragments and signatures do
/// not start the decode clock. `first_ttft_ms` is a different clock: it
/// stamps the first event of any kind, including `message_start`.
pub fn is_content_delta(delta: &Delta) -> bool {
    match delta {
        Delta::Text { text } => !text.is_empty(),
        Delta::Thinking { text } => !text.is_empty(),
        Delta::ToolInput { json } => !json.is_empty(),
        Delta::Signature { .. } => false,
    }
}

/// One constructed stream, in the shape the tests and the live watch share.
#[derive(Clone, Debug)]
pub struct StreamSample {
    pub query_source: String,
    pub streaming: bool,
    /// The stream ended before a `message_delta` usage. Preliminary
    /// `message_start` usage does not count.
    pub aborted_without_final_usage: bool,
    pub first_content_delta: Option<Duration>,
    pub message_stop: Option<Duration>,
    pub output_tokens: Option<u64>,
}

impl StreamSample {
    /// `Some(output_tokens, decode_ms)` when this request counts toward tps.
    pub fn accepted(&self) -> Option<(u64, u64)> {
        if !self.streaming
            || self.aborted_without_final_usage
            || !counts_for_footer(&self.query_source)
        {
            return None;
        }
        let (Some(start), Some(stop), Some(output_tokens)) = (
            self.first_content_delta,
            self.message_stop,
            self.output_tokens,
        ) else {
            return None;
        };
        let decode_ms = stop.saturating_sub(start).as_millis() as u64;
        if decode_ms < MIN_DECODE_MS {
            return None;
        }
        Some((output_tokens, decode_ms))
    }
}

/// Events a test (or a folded live stream) can replay.
#[derive(Clone, Debug)]
pub enum SampleEvent {
    /// `message_start` and anything else that is not content or `message_stop`.
    Other,
    ContentDelta,
    MessageStop,
    /// Cumulative `message_delta` usage. The last one is the final usage.
    FinalUsage(Usage),
}

/// Fold an event sequence into a [`StreamSample`]. Offsets are from the
/// request start. `streaming: false` is the non-streaming fallback.
pub fn sample_from_events(
    query_source: &str,
    streaming: bool,
    aborted_without_final_usage: bool,
    events: &[(Duration, SampleEvent)],
) -> StreamSample {
    let mut first_content_delta = None;
    let mut message_stop = None;
    let mut output_tokens = None;
    for (at, event) in events {
        match event {
            SampleEvent::ContentDelta if first_content_delta.is_none() => {
                first_content_delta = Some(*at);
            }
            SampleEvent::MessageStop if message_stop.is_none() => {
                message_stop = Some(*at);
            }
            SampleEvent::FinalUsage(usage) => {
                output_tokens = Some(usage.output_tokens);
            }
            SampleEvent::Other | SampleEvent::ContentDelta | SampleEvent::MessageStop => {}
        }
    }
    StreamSample {
        query_source: query_source.to_string(),
        streaming,
        aborted_without_final_usage,
        first_content_delta,
        message_stop,
        output_tokens,
    }
}

/// Live clock for one streaming response. Dropped at the end of the drain,
/// including abort and error returns, and commits tps then.
pub struct StreamDecodeWatch {
    query_source: String,
    first_content_delta: Option<Instant>,
    message_stop: Option<Instant>,
    output_tokens: Option<u64>,
}

impl StreamDecodeWatch {
    pub fn new(query_source: &str) -> Self {
        Self {
            query_source: query_source.to_string(),
            first_content_delta: None,
            message_stop: None,
            output_tokens: None,
        }
    }

    pub fn note_content_delta(&mut self, now: Instant) {
        if self.first_content_delta.is_none() {
            self.first_content_delta = Some(now);
        }
    }

    pub fn note_message_stop(&mut self, now: Instant) {
        if self.message_stop.is_none() {
            self.message_stop = Some(now);
        }
    }

    pub fn note_final_output_tokens(&mut self, output_tokens: u64) {
        self.output_tokens = Some(output_tokens);
    }

    fn sample(&self) -> StreamSample {
        let start = self.first_content_delta;
        StreamSample {
            query_source: self.query_source.clone(),
            streaming: true,
            aborted_without_final_usage: self.output_tokens.is_none(),
            first_content_delta: start.map(|_| Duration::ZERO),
            message_stop: match (start, self.message_stop) {
                (Some(start), Some(stop)) => Some(stop.saturating_duration_since(start)),
                _ => None,
            },
            output_tokens: self.output_tokens,
        }
    }
}

impl Drop for StreamDecodeWatch {
    fn drop(&mut self) {
        commit_tps_sample(&self.sample());
    }
}

fn commit_tps_sample(sample: &StreamSample) {
    let Some((output_tokens, decode_ms)) = sample.accepted() else {
        return;
    };
    with_bound_store(|store| {
        store.set_state(|prev| {
            let mut next = (**prev).clone();
            next.footer_metrics.accept_tps(output_tokens, decode_ms);
            UpdateDecision::Replace {
                next: std::sync::Arc::new(next),
                result: (),
            }
        });
    });
}

/// Cache sample for a finished main-thread request, streaming or not.
/// Non-streaming requests update cache and never tps (they never construct
/// a [`StreamDecodeWatch`]).
pub fn record_main_thread_cache(
    query_source: &str,
    model: &str,
    profile: Option<&str>,
    usage: &Usage,
) {
    if !counts_for_footer(query_source) {
        return;
    }
    let (input_price, read_price) = prices_or_zero(profile, model);
    with_bound_store(|store| {
        store.set_state(|prev| {
            let mut next = (**prev).clone();
            next.footer_metrics
                .observe_cache(model, usage, input_price, read_price);
            if next.footer_metrics == prev.footer_metrics {
                return UpdateDecision::Same(());
            }
            UpdateDecision::Replace {
                next: std::sync::Arc::new(next),
                result: (),
            }
        });
    });
}

fn prices_or_zero(profile: Option<&str>, model: &str) -> (f64, f64) {
    let settings = crate::utils::settings::get_initial_settings();
    let resolved = crate::utils::model::profile::resolve_request_profile(&settings, profile).ok();
    let profile_cost = resolved.as_ref().and_then(|resolved| {
        crate::utils::model::profile::model_profile(settings.nyan.as_deref()?, &resolved.profile)?
            .cost
            .as_ref()
    });
    match crate::utils::model::pricing::resolve_price(
        resolved.as_ref().map(|resolved| resolved.provider_kind),
        model,
        profile_cost,
    ) {
        Some(price) => (price.input, price.cache_read),
        None => (0.0, 0.0),
    }
}

pub fn start_user_turn(store: &AppStore) {
    store.set_state(|prev| {
        if prev.footer_metrics.turn_open && prev.footer_metrics.current_turn.is_empty() {
            return UpdateDecision::Same(());
        }
        let mut next = (**prev).clone();
        next.footer_metrics.start_turn();
        UpdateDecision::Replace {
            next: std::sync::Arc::new(next),
            result: (),
        }
    });
}

pub fn finish_user_turn(store: &AppStore) {
    store.set_state(|prev| {
        if !prev.footer_metrics.turn_open {
            return UpdateDecision::Same(());
        }
        let mut next = (**prev).clone();
        next.footer_metrics.finish_turn();
        UpdateDecision::Replace {
            next: std::sync::Arc::new(next),
            result: (),
        }
    });
    // @nyan: branch / worktree can change during a turn (checkout, commit).
    refresh_git(store, None);
}

/// Compaction, `/clear` and `/resume` all replace the request prefix; exempt
/// the next main-thread request from the cache-miss warning.
pub fn note_main_thread_prefix_reset(store: &AppStore) {
    store.set_state(|prev| {
        if prev.footer_metrics.suppress_cache_miss {
            return UpdateDecision::Same(());
        }
        let mut next = (**prev).clone();
        next.footer_metrics.note_compaction();
        UpdateDecision::Replace {
            next: std::sync::Arc::new(next),
            result: (),
        }
    });
}

/// `git-dir` and `git-common-dir` differ → linked worktree. Paths are
/// compared after lexical normalization, so a trailing slash does not count
/// as a difference.
pub fn is_linked_worktree(git_dir: &str, git_common_dir: &str) -> bool {
    let git_dir = normalize_git_path(git_dir);
    let common = normalize_git_path(git_common_dir);
    !git_dir.as_os_str().is_empty() && !common.as_os_str().is_empty() && git_dir != common
}

fn normalize_git_path(path: &str) -> PathBuf {
    let trimmed = path.trim().trim_matches('"');
    let mut output = PathBuf::new();
    for component in Path::new(trimmed).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if output.file_name().is_some_and(|name| name != "..") {
                    output.pop();
                } else if !output.has_root() {
                    output.push("..");
                }
            }
            other => output.push(other.as_os_str()),
        }
    }
    output
}

/// `Some(branch)` when HEAD is `ref: refs/heads/<name>`. Anything else
/// (detached sha, missing file) is detached.
pub fn branch_from_head(contents: &str) -> Option<String> {
    let rest = contents.trim().strip_prefix("ref:")?.trim();
    rest.strip_prefix("refs/heads/")
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

/// Build a snapshot from one `rev-parse` stdout (git-dir, git-common-dir,
/// short sha) and the `<git-dir>/HEAD` file.
pub fn git_snapshot_from_probe(
    git_dir: &str,
    git_common_dir: &str,
    short_sha: &str,
    head_contents: Option<&str>,
) -> GitSnapshot {
    let short_sha = short_sha.trim();
    if short_sha.is_empty() {
        return GitSnapshot {
            probed: true,
            ..GitSnapshot::default()
        };
    }
    let branch = head_contents.and_then(branch_from_head);
    GitSnapshot {
        probed: true,
        in_repo: true,
        detached: branch.is_none(),
        branch,
        short_sha: Some(short_sha.to_string()),
        linked_worktree: is_linked_worktree(git_dir, git_common_dir),
    }
}

/// Three non-empty lines: git-dir, git-common-dir, short sha.
pub fn parse_rev_parse_lines(stdout: &str) -> Option<(String, String, String)> {
    let lines: Vec<String> = stdout
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();
    if lines.len() < 3 {
        None
    } else {
        Some((lines[0].clone(), lines[1].clone(), lines[2].clone()))
    }
}

fn not_a_repo() -> GitSnapshot {
    GitSnapshot {
        probed: true,
        ..GitSnapshot::default()
    }
}

fn probe_git(cwd: &Path) -> GitSnapshot {
    let mut command = std::process::Command::new(crate::utils::git::git_exe());
    crate::utils::subprocess_env::apply_process_env_std(&mut command);
    // @nyan: one subprocess. `--abbrev-ref` is sticky and would replace
    // `--short`, so the branch is read from `<git-dir>/HEAD` — the same file
    // CC `getBranch` reads — instead of a second spawn.
    let output = command
        .arg("-C")
        .arg(cwd)
        .args([
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
            "--short=12",
            "HEAD",
        ])
        .output();
    let Ok(output) = output else {
        return not_a_repo();
    };
    if !output.status.success() {
        return not_a_repo();
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let Some((git_dir, common_dir, short_sha)) = parse_rev_parse_lines(&stdout) else {
        return not_a_repo();
    };
    let git_dir_path = resolve_against(cwd, &git_dir);
    let common_path = resolve_against(cwd, &common_dir);
    let head = std::fs::read_to_string(git_dir_path.join("HEAD")).ok();
    git_snapshot_from_probe(
        &git_dir_path.to_string_lossy(),
        &common_path.to_string_lossy(),
        &short_sha,
        head.as_deref(),
    )
}

fn resolve_against(cwd: &Path, raw: &str) -> PathBuf {
    let path = Path::new(raw.trim());
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

// Disabled under `cfg(test)` so REPL tests do not spawn git or race the
// store. Production binaries leave it on. The pure predicate is what the
// unit tests cover.
static GIT_REFRESH_ENABLED: AtomicBool = AtomicBool::new(cfg!(not(test)));
static GIT_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Main-thread tool context only.
///
/// Same check as `context.agent_id.is_none()` in compaction. Subagents keep
/// the parent [`AppStore`] (`create_subagent_context` reuses the root store),
/// so their branch and cwd must not replace the footer.
pub fn footer_git_allowed(agent_id: Option<&str>) -> bool {
    agent_id.is_none()
}

/// Probe branch, short sha and worktree in the background. Never call this
/// from a render body — it spawns a process.
pub fn refresh_git(store: &AppStore, cwd: Option<PathBuf>) {
    if !GIT_REFRESH_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let cwd = match cwd {
        Some(cwd) => {
            remember_git_cwd(store, &cwd);
            cwd
        }
        None => store
            .get()
            .footer_metrics
            .git_cwd
            .clone()
            .unwrap_or_else(crate::bootstrap::state::get_original_cwd),
    };
    let generation = GIT_GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    let store = store.clone();
    let _ = std::thread::Builder::new()
        .name("nyan-footer-git".into())
        .spawn(move || {
            let snapshot = probe_git(&cwd);
            if GIT_GENERATION.load(Ordering::Relaxed) != generation {
                return;
            }
            install_git(&store, snapshot, cwd);
        });
}

fn remember_git_cwd(store: &AppStore, cwd: &Path) {
    store.set_state(|prev| {
        if prev.footer_metrics.git_cwd.as_deref() == Some(cwd) {
            return UpdateDecision::Same(());
        }
        let mut next = (**prev).clone();
        next.footer_metrics.git_cwd = Some(cwd.to_path_buf());
        UpdateDecision::Replace {
            next: std::sync::Arc::new(next),
            result: (),
        }
    });
}

fn install_git(store: &AppStore, snapshot: GitSnapshot, cwd: PathBuf) {
    store.set_state(|prev| {
        if prev.footer_metrics.git == snapshot
            && prev.footer_metrics.git_cwd.as_deref() == Some(&cwd)
        {
            return UpdateDecision::Same(());
        }
        let mut next = (**prev).clone();
        next.footer_metrics.git = snapshot.clone();
        next.footer_metrics.git_cwd = Some(cwd.clone());
        UpdateDecision::Replace {
            next: std::sync::Arc::new(next),
            result: (),
        }
    });
}

/// The main thread's session cwd for display, without a `ToolUseContext`.
///
/// `ToolUseContext::effective_cwd` is the authority (`cwd_override`, updated
/// by a persistent Bash `cd`, else `get_original_cwd`). The render tree has no
/// context handle, but the stop-hook snapshot holds the last main-thread one.
/// Before the first turn (and whenever no snapshot exists) the session cwd is
/// the startup/worktree directory, which is the same fallback `effective_cwd`
/// uses. Read-only: this never probes git and never writes.
pub fn session_cwd() -> PathBuf {
    crate::utils::forked_agent::get_last_cache_safe_params()
        .map(|params| params.tool_use_context.effective_cwd())
        .unwrap_or_else(crate::bootstrap::state::get_original_cwd)
}

fn bound_store() -> &'static Mutex<Option<AppStore>> {
    static SLOT: OnceLock<Mutex<Option<AppStore>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// The interactive session publishes its store so the request layer, which
/// has no `ToolUseContext`, can write footer metrics.
pub fn bind_store(store: AppStore) {
    *bound_store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(store);
}

pub fn unbind_store() {
    *bound_store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

fn with_bound_store(f: impl FnOnce(&AppStore)) {
    let guard = bound_store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(store) = guard.as_ref() {
        f(store);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::app_state_store::AppState;

    fn usage(input: u64, output: u64, cache_read: u64, cache_write: u64) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cache_read,
            cache_write_tokens: cache_write,
            ..Usage::default()
        }
    }

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    fn main_stream(events: &[(Duration, SampleEvent)]) -> StreamSample {
        sample_from_events("repl_main_thread", true, false, events)
    }

    fn apply(metrics: &mut FooterMetrics, sample: &StreamSample) {
        if let Some((tokens, decode_ms)) = sample.accepted() {
            metrics.accept_tps(tokens, decode_ms);
        }
    }

    fn counted_events(output_tokens: u64, decode_ms: u64) -> Vec<(Duration, SampleEvent)> {
        vec![
            (ms(0), SampleEvent::Other),
            (ms(10), SampleEvent::ContentDelta),
            (
                ms(10 + decode_ms),
                SampleEvent::FinalUsage(usage(10, output_tokens, 0, 0)),
            ),
            (ms(10 + decode_ms), SampleEvent::MessageStop),
        ]
    }

    #[test]
    fn tps_sums_several_main_thread_requests_before_dividing() {
        let mut metrics = FooterMetrics::default();
        apply(&mut metrics, &main_stream(&counted_events(100, 1_000)));
        apply(&mut metrics, &main_stream(&counted_events(300, 1_000)));
        assert_eq!(metrics.current_turn.output_tokens, 400);
        assert_eq!(metrics.current_turn.decode_ms, 2_000);
        assert!((metrics.current_turn.tokens_per_second().unwrap() - 200.0).abs() < 1e-9);
    }

    #[test]
    fn tps_ignores_abort_without_final_usage() {
        let mut metrics = FooterMetrics::default();
        let events = [
            (ms(0), SampleEvent::Other),
            (ms(5), SampleEvent::ContentDelta),
            (ms(800), SampleEvent::MessageStop),
        ];
        let sample = sample_from_events("repl_main_thread", true, true, &events);
        apply(&mut metrics, &sample);
        assert!(metrics.current_turn.is_empty());
    }

    #[test]
    fn tps_ignores_non_streaming_requests() {
        let mut metrics = FooterMetrics::default();
        let sample = sample_from_events("repl_main_thread", false, false, &counted_events(80, 500));
        apply(&mut metrics, &sample);
        assert!(metrics.current_turn.is_empty());
    }

    #[test]
    fn tps_ignores_windows_under_200ms() {
        let mut metrics = FooterMetrics::default();
        apply(&mut metrics, &main_stream(&counted_events(40, 199)));
        assert!(metrics.current_turn.is_empty());
        apply(&mut metrics, &main_stream(&counted_events(40, 200)));
        assert_eq!(metrics.current_turn.decode_ms, 200);
        assert_eq!(metrics.current_turn.output_tokens, 40);
    }

    #[test]
    fn tps_ignores_subagents_and_cache_sharing_forks() {
        let mut metrics = FooterMetrics::default();
        for source in [
            "agent:builtin:explore",
            "agent:custom",
            "agent",
            "hook_agent",
            "compact",
            "cache_warm",
            "extract_memories",
            "sdk",
        ] {
            let sample = sample_from_events(source, true, false, &counted_events(90, 500));
            apply(&mut metrics, &sample);
        }
        assert!(metrics.current_turn.is_empty());
    }

    #[test]
    fn message_start_usage_is_not_final() {
        let events = [
            (ms(0), SampleEvent::FinalUsage(usage(1_000, 0, 0, 0))),
            (ms(20), SampleEvent::ContentDelta),
            (ms(700), SampleEvent::MessageStop),
        ];
        // A message_start that was wrongly labeled final would count. The
        // abort flag is how the drain reports "never saw message_delta".
        let aborted = sample_from_events("repl_main_thread", true, true, &events);
        assert!(aborted.accepted().is_none());
    }

    #[test]
    fn new_user_turn_keeps_the_previous_sample_and_clears_the_current_one() {
        let mut metrics = FooterMetrics::default();
        apply(&mut metrics, &main_stream(&counted_events(100, 1_000)));
        metrics.start_turn();
        assert!(metrics.current_turn.is_empty());
        assert_eq!(metrics.previous_turn.output_tokens, 100);
        assert!(metrics.turn_open);
        assert_eq!(metrics.displayed_turn().unwrap().output_tokens, 100);
        apply(&mut metrics, &main_stream(&counted_events(50, 500)));
        assert_eq!(metrics.displayed_turn().unwrap().output_tokens, 50);
        metrics.finish_turn();
        assert!(!metrics.turn_open);
        metrics.start_turn();
        assert_eq!(metrics.previous_turn.output_tokens, 50);
        assert!(metrics.current_turn.is_empty());
    }

    #[test]
    fn empty_turn_does_not_clobber_the_previous_sample() {
        let mut metrics = FooterMetrics::default();
        apply(&mut metrics, &main_stream(&counted_events(100, 1_000)));
        metrics.start_turn();
        metrics.finish_turn();
        metrics.start_turn();
        assert_eq!(metrics.previous_turn.output_tokens, 100);
        assert!(metrics.current_turn.is_empty());
    }

    #[test]
    fn content_delta_is_text_thinking_or_tool_input_only() {
        assert!(is_content_delta(&Delta::Text { text: "a".into() }));
        assert!(is_content_delta(&Delta::Thinking { text: "a".into() }));
        assert!(is_content_delta(&Delta::ToolInput { json: "{".into() }));
        assert!(!is_content_delta(&Delta::Text {
            text: String::new()
        }));
        assert!(!is_content_delta(&Delta::Thinking {
            text: String::new()
        }));
        assert!(!is_content_delta(&Delta::ToolInput {
            json: String::new()
        }));
        assert!(!is_content_delta(&Delta::Signature {
            signature: "sig".into()
        }));
    }

    #[test]
    fn watch_drop_records_only_an_accepted_main_thread_stream() {
        let store = AppStore::new(AppState::default(), None);
        bind_store(store.clone());
        {
            let mut watch = StreamDecodeWatch::new("repl_main_thread");
            let start = Instant::now();
            watch.note_content_delta(start);
            watch.note_message_stop(start + ms(500));
            watch.note_final_output_tokens(120);
        }
        assert_eq!(store.get().footer_metrics.current_turn.output_tokens, 120);
        assert_eq!(store.get().footer_metrics.current_turn.decode_ms, 500);

        {
            let mut watch = StreamDecodeWatch::new("agent:builtin:explore");
            let start = Instant::now();
            watch.note_content_delta(start);
            watch.note_message_stop(start + ms(500));
            watch.note_final_output_tokens(999);
        }
        assert_eq!(store.get().footer_metrics.current_turn.output_tokens, 120);

        {
            let mut watch = StreamDecodeWatch::new("repl_main_thread");
            let start = Instant::now();
            watch.note_content_delta(start);
            watch.note_message_stop(start + ms(50));
            watch.note_final_output_tokens(10);
        }
        assert_eq!(store.get().footer_metrics.current_turn.output_tokens, 120);
        unbind_store();
    }

    fn observe(metrics: &mut FooterMetrics, model: &str, usage: Usage) {
        metrics.observe_cache(model, &usage, 0.0, 0.0);
    }

    #[test]
    fn cache_display_gate_opens_only_after_a_cache_field_is_reported() {
        let mut metrics = FooterMetrics::default();
        observe(&mut metrics, "m", usage(100, 10, 0, 0));
        assert!(!metrics.cache.session_reported_cache);
        assert_eq!(metrics.cache.hit_rate, Some(0.0));

        observe(&mut metrics, "m", usage(40, 10, 60, 0));
        assert!(metrics.cache.session_reported_cache);
        assert!((metrics.cache.hit_rate.unwrap() - 0.6).abs() < 1e-9);

        observe(&mut metrics, "m", usage(100, 10, 0, 0));
        assert!(metrics.cache.session_reported_cache);
    }

    #[test]
    fn cache_write_alone_opens_the_display_gate() {
        let mut metrics = FooterMetrics::default();
        observe(&mut metrics, "m", usage(80, 10, 0, 20));
        assert!(metrics.cache.session_reported_cache);
        assert!((metrics.cache.hit_rate.unwrap() - 0.0).abs() < 1e-9);
    }

    #[test]
    fn first_request_does_not_warn_even_when_the_miss_would_be_large() {
        let mut metrics = FooterMetrics::default();
        observe(&mut metrics, "m", usage(5_000, 10, 0, 0));
        assert!(!metrics.cache.miss_warning);
    }

    #[test]
    fn significant_miss_warns_until_a_later_healthy_request() {
        let mut metrics = FooterMetrics::default();
        observe(&mut metrics, "m", usage(5_000, 10, 0, 0));
        observe(&mut metrics, "m", usage(4_000, 10, 1_000, 0));
        assert!(metrics.cache.miss_warning);
        observe(&mut metrics, "m", usage(0, 10, 5_000, 0));
        assert!(!metrics.cache.miss_warning);
    }

    #[test]
    fn noise_floor_miss_does_not_warn() {
        let mut metrics = FooterMetrics::default();
        observe(&mut metrics, "m", usage(1_800, 10, 0, 0));
        // prompt 1800, read 1500, missed 300 <= 1024.
        observe(&mut metrics, "m", usage(300, 10, 1_500, 0));
        assert!(!metrics.cache.miss_warning);
    }

    #[test]
    fn compaction_and_model_switch_suppress_the_next_warning_only() {
        let mut metrics = FooterMetrics::default();
        observe(&mut metrics, "m", usage(5_000, 10, 0, 0));
        metrics.note_compaction();
        observe(&mut metrics, "m", usage(4_000, 10, 1_000, 0));
        assert!(!metrics.cache.miss_warning);

        observe(&mut metrics, "m", usage(4_000, 10, 1_000, 0));
        assert!(metrics.cache.miss_warning);

        observe(&mut metrics, "other", usage(4_000, 10, 1_000, 0));
        assert!(
            metrics.cache.miss_warning,
            "exempt request keeps the warning"
        );
        observe(&mut metrics, "other", usage(4_000, 10, 1_000, 0));
        assert!(metrics.cache.miss_warning);
        observe(&mut metrics, "other", usage(0, 10, 5_000, 0));
        assert!(!metrics.cache.miss_warning);
    }

    #[test]
    fn unknown_price_still_detects_a_token_miss() {
        let mut metrics = FooterMetrics::default();
        metrics.observe_cache("m", &usage(5_000, 10, 0, 0), 0.0, 0.0);
        metrics.observe_cache("m", &usage(4_000, 10, 1_000, 0), 0.0, 0.0);
        assert!(metrics.cache.miss_warning);
    }

    #[test]
    fn prefix_reset_suppresses_the_next_cache_warning_via_store() {
        let store = AppStore::new(AppState::default(), None);
        bind_store(store.clone());
        record_main_thread_cache("repl_main_thread", "m", None, &usage(5_000, 10, 0, 0));
        record_main_thread_cache("repl_main_thread", "m", None, &usage(4_000, 10, 1_000, 0));
        assert!(store.get().footer_metrics.cache.miss_warning);

        record_main_thread_cache("repl_main_thread", "m", None, &usage(0, 10, 5_000, 0));
        assert!(!store.get().footer_metrics.cache.miss_warning);

        note_main_thread_prefix_reset(&store);
        record_main_thread_cache("repl_main_thread", "m", None, &usage(4_000, 10, 1_000, 0));
        assert!(!store.get().footer_metrics.cache.miss_warning);
        unbind_store();
    }

    #[test]
    fn subagent_sources_do_not_count_as_main_thread() {
        assert!(counts_for_footer("repl_main_thread"));
        assert!(!counts_for_footer("sdk"));
        assert!(!counts_for_footer("agent:builtin:explore"));
        assert!(!counts_for_footer("compact"));
        assert!(!counts_for_footer("cache_warm"));
    }

    #[test]
    fn linked_worktree_is_a_pure_path_comparison() {
        assert!(!is_linked_worktree(".git", ".git"));
        assert!(!is_linked_worktree("/repo/.git", "/repo/.git/"));
        assert!(!is_linked_worktree("/repo/./.git", "/repo/.git"));
        assert!(is_linked_worktree(
            "/repo/.git/worktrees/nyan-tui-metrics",
            "/repo/.git"
        ));
        assert!(!is_linked_worktree("", ""));
    }

    #[test]
    fn head_file_and_rev_parse_lines_build_the_snapshot() {
        let (git_dir, common, sha) =
            parse_rev_parse_lines("/repo/.git/worktrees/feature\n/repo/.git\nabc123def456\n")
                .unwrap();
        let on_branch = git_snapshot_from_probe(
            &git_dir,
            &common,
            &sha,
            Some("ref: refs/heads/feature/footer\n"),
        );
        assert!(on_branch.in_repo);
        assert!(!on_branch.detached);
        assert_eq!(on_branch.branch.as_deref(), Some("feature/footer"));
        assert_eq!(on_branch.short_sha.as_deref(), Some("abc123def456"));
        assert!(on_branch.linked_worktree);

        let detached = git_snapshot_from_probe(
            "/repo/.git",
            "/repo/.git",
            "abc123def456",
            Some("abc123def4567890abcd\n"),
        );
        assert!(detached.detached);
        assert!(detached.branch.is_none());
        assert!(!detached.linked_worktree);
        assert_eq!(
            branch_from_head("ref: refs/heads/master"),
            Some("master".into())
        );
        assert_eq!(branch_from_head("abc123"), None);
    }

    #[test]
    fn store_turn_helpers_rotate_and_close() {
        let store = AppStore::new(AppState::default(), None);
        bind_store(store.clone());
        {
            let mut watch = StreamDecodeWatch::new("repl_main_thread");
            let start = Instant::now();
            watch.note_content_delta(start);
            watch.note_message_stop(start + ms(400));
            watch.note_final_output_tokens(80);
        }
        start_user_turn(&store);
        let metrics = store.get();
        assert!(metrics.footer_metrics.turn_open);
        assert!(metrics.footer_metrics.current_turn.is_empty());
        assert_eq!(metrics.footer_metrics.previous_turn.output_tokens, 80);
        finish_user_turn(&store);
        assert!(!store.get().footer_metrics.turn_open);
        unbind_store();
    }

    #[test]
    fn subagent_context_does_not_stage_footer_git_cwd() {
        let store = AppStore::new(AppState::default(), None);
        let cwd = PathBuf::from("/tmp/agent-worktree");
        // `refresh_git` returns before `remember_git_cwd` while the probe is
        // disabled. The call sites use `footer_git_allowed` (agent id is
        // `None`) as that gate, so exercise the mutation it protects.
        if footer_git_allowed(Some("subagent")) {
            remember_git_cwd(&store, &cwd);
        }
        assert!(store.get().footer_metrics.git_cwd.is_none());
        assert!(!footer_git_allowed(Some("subagent")));

        assert!(footer_git_allowed(None));
        if footer_git_allowed(None) {
            remember_git_cwd(&store, &cwd);
        }
        assert_eq!(
            store.get().footer_metrics.git_cwd.as_deref(),
            Some(cwd.as_path())
        );
    }
}
