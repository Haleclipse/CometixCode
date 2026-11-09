//! @nyan: post-hoc usage probes (decision-provider §7).
//!
//! A decision can only record what it answered. Whether the retained result
//! was actually used afterwards is observable from later events: the model
//! reads a file candidate with `Read`, fetches a URL candidate with
//! `WebFetch`, or the user accepts the shown prompt suggestion. This module
//! keeps the recent probes in memory and appends two extra record shapes to
//! the same `decision/decisions.jsonl`:
//!
//! - `{"event":"probe", "probeId", "point", "kind", "candidateCount"}` when a
//!   decision retains candidates (immediately after the decision record);
//! - `{"event":"posthoc", "probeId", "point", "kind", "identity"}` the first
//!   time each retained candidate is observed.
//!
//! `/decision` reads `probes` and `used` from these rows, so the denominator
//! is explicit and decision samples are never double-counted. Probes expire
//! after [`PROBE_TTL`]; an expired probe simply stops matching, keeping memory
//! bounded across a long session. `/clear` forgets them (`clear`).
//!
//! Scope: only points whose retained result has a concrete identity (a file
//! path, a URL, or the suggestion text the user saw) create probes. Memory and
//! compaction points have no such identity yet and are not probed here.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::DecisionPoint;

/// How long a retained candidate can still be attributed to its decision.
const PROBE_TTL: Duration = Duration::from_secs(30 * 60);
/// Upper bound on pending probes; the oldest is dropped first.
const MAX_PROBES: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    File,
    Url,
    Suggestion,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::File => "file",
            Kind::Url => "url",
            Kind::Suggestion => "suggestion",
        }
    }
}

struct Candidate {
    key: String,
    label: String,
    used: bool,
}

struct Probe {
    id: String,
    point: DecisionPoint,
    kind: Kind,
    created: Instant,
    candidates: Vec<Candidate>,
}

fn probes() -> &'static Mutex<Vec<Probe>> {
    static PROBES: OnceLock<Mutex<Vec<Probe>>> = OnceLock::new();
    PROBES.get_or_init(|| Mutex::new(Vec::new()))
}

fn lock() -> MutexGuard<'static, Vec<Probe>> {
    probes()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProbeRecord<'a> {
    timestamp: String,
    event: &'static str,
    probe_id: &'a str,
    point: &'a str,
    kind: &'a str,
    candidate_count: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PosthocRecord {
    timestamp: String,
    event: &'static str,
    probe_id: String,
    point: &'static str,
    kind: &'static str,
    identity: String,
}

/// Register the file candidates a decision retained. Paths are matched later
/// against `Read` calls after canonicalization, so callers may pass either an
/// absolute path or a path relative to the current directory.
pub fn register_files<I>(point: DecisionPoint, paths: I) -> Option<String>
where
    I: IntoIterator<Item = PathBuf>,
{
    register(
        point,
        Kind::File,
        paths.into_iter().map(|path| {
            let key = normalize_file(&path);
            let label = path.display().to_string();
            (key, label)
        }),
    )
}

/// Register the URL candidates a decision retained. URLs are matched later
/// against `WebFetch` calls after scheme/host and fragment normalization.
pub fn register_urls<I>(point: DecisionPoint, urls: I) -> Option<String>
where
    I: IntoIterator<Item = String>,
{
    register(
        point,
        Kind::Url,
        urls.into_iter().map(|url| {
            let key = normalize_url(&url);
            (key, url)
        }),
    )
}

/// Register the suggestion a decision let through. Acceptance later marks it
/// used, so `/decision` reports how often generated suggestions were taken.
pub fn register_suggestion(point: DecisionPoint, text: &str) -> Option<String> {
    let key = normalize_suggestion(text);
    register(point, Kind::Suggestion, std::iter::once((key.clone(), key)))
}

fn register<I>(point: DecisionPoint, kind: Kind, candidates: I) -> Option<String>
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut seen = HashSet::new();
    let candidates = candidates
        .into_iter()
        .filter(|(key, _)| !key.is_empty() && seen.insert(key.clone()))
        .map(|(key, label)| Candidate {
            key,
            label,
            used: false,
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return None;
    }

    let candidate_count = candidates.len();
    let id = uuid::Uuid::new_v4().simple().to_string();
    let now = Instant::now();
    {
        let mut pending = lock();
        pending.retain(|probe| now.duration_since(probe.created) < PROBE_TTL);
        while pending.len() >= MAX_PROBES {
            pending.remove(0);
        }
        pending.push(Probe {
            id: id.clone(),
            point,
            kind,
            created: now,
            candidates,
        });
    }
    append(&ProbeRecord {
        timestamp: chrono::Utc::now().to_rfc3339(),
        event: "probe",
        probe_id: &id,
        point: point.as_str(),
        kind: kind.as_str(),
        candidate_count,
    });
    Some(id)
}

/// Report a `Read` of `path`. Every live file probe that retained the path is
/// marked used; the first observation of each candidate appends one row.
pub fn observe_file_read(path: &str) {
    observe(Kind::File, normalize_file(Path::new(path)));
}

/// Report a `WebFetch` of `url`. Every live URL probe that retained the
/// candidate is marked used.
pub fn observe_url(url: &str) {
    observe(Kind::Url, normalize_url(url));
}

/// Report that the user accepted `text` as the shown prompt suggestion.
pub fn observe_suggestion_acceptance(text: &str) {
    observe(Kind::Suggestion, normalize_suggestion(text));
}

fn observe(kind: Kind, key: String) {
    if key.is_empty() {
        return;
    }
    let now = Instant::now();
    let mut records = Vec::new();
    {
        let mut pending = lock();
        pending.retain(|probe| now.duration_since(probe.created) < PROBE_TTL);
        for probe in pending.iter_mut() {
            if probe.kind != kind {
                continue;
            }
            for candidate in &mut probe.candidates {
                if candidate.used || candidate.key != key {
                    continue;
                }
                candidate.used = true;
                records.push(PosthocRecord {
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    event: "posthoc",
                    probe_id: probe.id.clone(),
                    point: probe.point.as_str(),
                    kind: kind.as_str(),
                    identity: candidate.label.clone(),
                });
            }
        }
    }
    for record in records {
        append(&record);
    }
}

fn append<T: Serialize>(record: &T) {
    if let Err(error) = crate::services::decision::log::append(record) {
        crate::utils::debug::log_for_debugging(&format!(
            "DecisionProvider: cannot append post-hoc probe: {error}"
        ));
    }
}

/// `/clear`: a new conversation must not attribute old reads to old decisions.
pub fn clear() {
    lock().clear();
}

fn normalize_file(path: &Path) -> String {
    let normalized = std::fs::canonicalize(path).unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(path))
                .unwrap_or_else(|_| path.to_path_buf())
        }
    });
    normalized.to_string_lossy().into_owned()
}

/// Suggestions are matched by their visible text. Whitespace is collapsed so
/// a re-rendered row still matches the generated string, but case is kept:
/// suggestion style is part of the identity the user accepted.
fn normalize_suggestion(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_url(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    match url::Url::parse(trimmed) {
        Ok(mut parsed) => {
            // Fragments never reach the server and search providers disagree
            // on trailing slashes; both must not split one candidate in two.
            parsed.set_fragment(None);
            let path = parsed.path().to_string();
            if path != "/" && path.ends_with('/') {
                parsed.set_path(path.trim_end_matches('/'));
            }
            parsed.to_string()
        }
        Err(_) => trimmed.trim_end_matches('/').to_string(),
    }
}

#[cfg(test)]
pub fn pending_count() -> usize {
    lock().len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use serde_json::Value;

    fn isolated_config(name: &str) -> (PathBuf, EnvVarGuard) {
        let dir = crate::utils::node_os::tmpdir().join(format!(
            "nyan-decision-probe-{name}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let guard = EnvVarGuard::set("NYAN_CONFIG_DIR", &dir);
        clear();
        (dir, guard)
    }

    fn records() -> Vec<Value> {
        let text =
            std::fs::read_to_string(crate::services::decision::log::log_path()).unwrap_or_default();
        text.lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .collect()
    }

    fn events(event: &str) -> Vec<Value> {
        records()
            .into_iter()
            .filter(|record| record["event"] == event)
            .collect()
    }

    #[test]
    fn a_file_candidate_is_matched_once_after_canonicalization() {
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (config, _env) = isolated_config("file");
        let file = config.join("src/main.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "fn main() {}").unwrap();

        let id = register_files(DecisionPoint::RetrievalAnswers, [file.clone()]).expect("probe");
        // A lexical variant still canonicalizes to the same candidate.
        observe_file_read(&config.join("src/./main.rs").display().to_string());
        observe_file_read(&file.display().to_string());

        let probes = events("probe");
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0]["probeId"], id);
        assert_eq!(probes[0]["point"], "retrieval.answers");
        assert_eq!(probes[0]["kind"], "file");
        assert_eq!(probes[0]["candidateCount"], 1);

        let posthoc = events("posthoc");
        assert_eq!(
            posthoc.len(),
            1,
            "one row per candidate, not per observation"
        );
        assert_eq!(posthoc[0]["probeId"], id);
        assert_eq!(posthoc[0]["point"], "retrieval.answers");
        assert_eq!(posthoc[0]["identity"], file.display().to_string());
        clear();
        let _ = std::fs::remove_dir_all(&config);
    }

    #[test]
    fn suggestion_acceptance_matches_once_after_whitespace_normalization() {
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (config, _env) = isolated_config("suggestion");
        assert!(register_suggestion(DecisionPoint::PromptSuggestionGenerateGate, "  ").is_none());
        let id = register_suggestion(
            DecisionPoint::PromptSuggestionGenerateGate,
            "run   the tests",
        )
        .expect("probe");
        observe_suggestion_acceptance("run the tests");
        observe_suggestion_acceptance("run the tests");

        let probes = events("probe");
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0]["probeId"], id);
        assert_eq!(probes[0]["point"], "prompt_suggestion.generate_gate");
        assert_eq!(probes[0]["kind"], "suggestion");

        let posthoc = events("posthoc");
        assert_eq!(posthoc.len(), 1, "one row per accepted suggestion");
        assert_eq!(posthoc[0]["point"], "prompt_suggestion.generate_gate");
        assert_eq!(posthoc[0]["kind"], "suggestion");
        assert_eq!(posthoc[0]["identity"], "run the tests");
        clear();
        let _ = std::fs::remove_dir_all(&config);
    }

    #[test]
    fn url_matching_ignores_fragments_case_and_a_trailing_slash() {
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (config, _env) = isolated_config("url");
        let id = register_urls(
            DecisionPoint::WebAnswers,
            ["https://Example.com/docs/page/?q=1#section".to_string()],
        )
        .expect("probe");
        observe_url("https://example.com/docs/page?q=1");

        let posthoc = events("posthoc");
        assert_eq!(posthoc.len(), 1);
        assert_eq!(posthoc[0]["probeId"], id);
        assert_eq!(posthoc[0]["kind"], "url");
        assert_eq!(
            posthoc[0]["identity"],
            "https://Example.com/docs/page/?q=1#section"
        );
        clear();
        let _ = std::fs::remove_dir_all(&config);
    }

    #[test]
    fn observing_another_kind_does_not_mark_a_probe() {
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (config, _env) = isolated_config("kind");
        register_files(DecisionPoint::RetrievalAnswers, [config.join("src/lib.rs")])
            .expect("probe");
        observe_url("https://example.com/");
        assert!(events("posthoc").is_empty());
        clear();
        let _ = std::fs::remove_dir_all(&config);
    }

    #[test]
    fn duplicate_candidates_are_kept_once_and_empty_registrations_log_nothing() {
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (config, _env) = isolated_config("dedupe");
        let url = "https://example.com/a".to_string();
        assert!(register_urls(DecisionPoint::WebAnswers, []).is_none());
        assert!(
            register_urls(
                DecisionPoint::WebAnswers,
                [url.clone(), format!("{url}/#x")]
            )
            .is_some()
        );
        let probes = events("probe");
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0]["candidateCount"], 1);
        clear();
        let _ = std::fs::remove_dir_all(&config);
    }

    #[test]
    fn expired_probes_stop_matching_and_are_evicted() {
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (config, _env) = isolated_config("ttl");
        let url = "https://example.com/ttl";
        {
            let mut pending = lock();
            pending.push(Probe {
                id: "expired".to_string(),
                point: DecisionPoint::WebAnswers,
                kind: Kind::Url,
                created: Instant::now()
                    .checked_sub(PROBE_TTL)
                    .expect("instant supports subtracting the TTL"),
                candidates: vec![Candidate {
                    key: normalize_url(url),
                    label: url.to_string(),
                    used: false,
                }],
            });
        }
        observe_url(url);
        assert!(
            events("posthoc").is_empty(),
            "an expired probe cannot match"
        );
        assert_eq!(pending_count(), 0, "observation evicts expired probes");
        clear();
        let _ = std::fs::remove_dir_all(&config);
    }

    #[test]
    fn the_oldest_probe_is_dropped_at_the_cap() {
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (config, _env) = isolated_config("cap");
        let url = |index: usize| format!("https://example.com/cap/{index}");
        for index in 0..=MAX_PROBES {
            register_urls(DecisionPoint::WebAnswers, [url(index)]).expect("probe");
        }
        assert_eq!(pending_count(), MAX_PROBES);
        // The first registration was evicted, the newest one is still live.
        observe_url(&url(0));
        assert!(events("posthoc").is_empty());
        observe_url(&url(MAX_PROBES));
        let posthoc = events("posthoc");
        assert_eq!(posthoc.len(), 1);
        assert_eq!(posthoc[0]["identity"], url(MAX_PROBES));
        clear();
        let _ = std::fs::remove_dir_all(&config);
    }

    #[test]
    fn clear_forgets_pending_probes() {
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (config, _env) = isolated_config("clear");
        register_urls(
            DecisionPoint::WebAnswers,
            ["https://example.com/a".to_string()],
        )
        .expect("probe");
        assert_eq!(pending_count(), 1);
        clear();
        assert_eq!(pending_count(), 0);
        observe_url("https://example.com/a");
        assert!(events("posthoc").is_empty());
        let _ = std::fs::remove_dir_all(&config);
    }
}
