//! @nyan: `/decision` — inspect the Decision Provider log
//! (docs/nyan/decision-provider.md §7).
//!
//! Reads `$NYAN_CONFIG_DIR/decision/decisions.jsonl` and prints one row per
//! point: sample count, answered/fallback outcomes, agreement counts where the
//! record carries them, and p50/p95 latency. It never calls the backend.

use crate::commands::Command;
use crate::utils::process_user_input::ProcessUserInputBaseResult;
use crate::utils::process_user_input::process_slash_command::local_command_result;

pub const NAME: &str = "decision";
pub const DESCRIPTION: &str = "Inspect Decision Provider samples, agreement, and latency";

pub fn command() -> Command {
    Command::local(NAME, DESCRIPTION)
        .argument_hint("[point]")
        .supports_non_interactive()
        .executable(call)
}

pub fn call(
    command: &Command,
    args: &str,
    uuid: Option<String>,
    _context: &crate::tool::ToolUseContext,
) -> ProcessUserInputBaseResult {
    let (output, is_error) = match summarize(args.trim()) {
        Ok(output) => (output, false),
        Err(error) => (error, true),
    };
    local_command_result(
        uuid,
        crate::commands::get_command_name(command),
        args,
        output,
        is_error,
        None,
    )
}

#[derive(Default)]
struct PointStats {
    samples: usize,
    answered: usize,
    fallback: usize,
    agree: usize,
    disagree: usize,
    /// `event: "probe"` rows: decisions that retained candidates.
    probes: usize,
    /// Probe ids that a later Read/WebFetch matched (`event: "posthoc"`).
    used_probes: std::collections::BTreeSet<String>,
    latencies: Vec<u64>,
}

impl PointStats {
    fn percentile(&mut self, percentile: f64) -> u64 {
        if self.latencies.is_empty() {
            return 0;
        }
        self.latencies.sort_unstable();
        let index = ((self.latencies.len() as f64 - 1.0) * percentile).round() as usize;
        self.latencies[index]
    }
}

fn summarize(filter: &str) -> Result<String, String> {
    let path = crate::services::decision::log::log_path();
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(format!(
                "No decision records yet (log: {}).",
                path.display()
            ));
        }
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };

    let mut by_point = std::collections::BTreeMap::<String, PointStats>::new();
    for line in content.lines() {
        let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(point) = record.get("point").and_then(|value| value.as_str()) else {
            continue;
        };
        if !filter.is_empty() && point != filter {
            continue;
        }
        let stats = by_point.entry(point.to_string()).or_default();
        // Post-hoc rows share the file but are not decisions: counting them as
        // samples would double-count and lower every latency percentile.
        match record.get("event").and_then(|value| value.as_str()) {
            Some("probe") => {
                stats.probes += 1;
                continue;
            }
            Some("posthoc") => {
                if let Some(id) = record.get("probeId").and_then(|value| value.as_str()) {
                    stats.used_probes.insert(id.to_string());
                }
                continue;
            }
            Some(_) => continue,
            None => {}
        }
        stats.samples += 1;
        match record.get("outcome").and_then(|value| value.as_str()) {
            Some("fallback") => stats.fallback += 1,
            _ => stats.answered += 1,
        }
        if let Some(latency) = record.get("latencyMs").and_then(|value| value.as_u64()) {
            stats.latencies.push(latency);
        }
        if let Some(agree) = record
            .get("detail")
            .and_then(|detail| detail.get("agrees"))
            .and_then(|value| value.as_bool())
        {
            if agree {
                stats.agree += 1;
            } else {
                stats.disagree += 1;
            }
        }
        let agreed = record.get("agreedCount").and_then(|value| value.as_u64());
        let differed = record.get("differedCount").and_then(|value| value.as_u64());
        if agreed.is_some() || differed.is_some() {
            stats.agree += agreed.unwrap_or(0) as usize;
            stats.disagree += differed.unwrap_or(0) as usize;
        }
    }

    if by_point.is_empty() {
        return Ok(format!(
            "No decision records for {} (log: {}).",
            if filter.is_empty() {
                "any point"
            } else {
                filter
            },
            path.display()
        ));
    }

    let mut output = format!("Decision log: {}\n\n", path.display());
    output.push_str(
        "point                            n  answered fallback agree disagree   p50   p95 probes  used\n",
    );
    for (point, mut stats) in by_point {
        let p50 = stats.percentile(0.50);
        let p95 = stats.percentile(0.95);
        output.push_str(&format!(
            "{point:<30} {:>3} {:>8} {:>8} {:>5} {:>8} {:>5} {:>5} {:>6} {:>5}\n",
            stats.samples,
            stats.answered,
            stats.fallback,
            stats.agree,
            stats.disagree,
            p50,
            p95,
            stats.probes,
            stats.used_probes.len(),
        ));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn summarize_groups_points_and_counts_agreement() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!(
            "nyan-decision-cmd-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(root.join("decision")).unwrap();
        let _config = EnvVarGuard::set("NYAN_CONFIG_DIR", &root);
        std::fs::write(
            root.join("decision/decisions.jsonl"),
            concat!(
                r#"{"point":"memory.type","outcome":"shadow","latencyMs":10,"detail":{"agrees":true}}"#,
                "\n",
                r#"{"point":"memory.type","outcome":"fallback","latencyMs":30,"detail":{"agrees":false}}"#,
                "\n",
                r#"{"point":"compaction.keep","outcome":"shadow","latencyMs":20,"agreedCount":2,"differedCount":1}"#,
                "\n"
            ),
        )
        .unwrap();

        let text = summarize("").expect("summary");
        assert!(text.contains("memory.type"));
        assert!(text.contains("compaction.keep"));
        assert!(text.contains(" 2 "));
        assert!(summarize("memory.type").unwrap().contains("memory.type"));
        assert!(
            !summarize("memory.type")
                .unwrap()
                .contains("compaction.keep")
        );
        assert!(
            summarize("missing.point")
                .unwrap()
                .contains("No decision records")
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn probe_rows_are_not_samples_and_used_counts_distinct_probes() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root = crate::utils::node_os::tmpdir().join(format!(
            "nyan-decision-probe-cmd-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(root.join("decision")).unwrap();
        let _config = EnvVarGuard::set("NYAN_CONFIG_DIR", &root);
        std::fs::write(
            root.join("decision/decisions.jsonl"),
            concat!(
                r#"{"point":"retrieval.answers","outcome":"answered","latencyMs":40,"detail":{"agrees":true}}"#,
                "\n",
                r#"{"point":"retrieval.answers","event":"probe","probeId":"p1","kind":"file","candidateCount":2}"#,
                "\n",
                r#"{"point":"retrieval.answers","event":"probe","probeId":"p2","kind":"file","candidateCount":1}"#,
                "\n",
                r#"{"point":"retrieval.answers","event":"posthoc","probeId":"p1","kind":"file","identity":"src/a.rs"}"#,
                "\n",
                r#"{"point":"retrieval.answers","event":"posthoc","probeId":"p1","kind":"file","identity":"src/b.rs"}"#,
                "\n"
            ),
        )
        .unwrap();

        let text = summarize("retrieval.answers").expect("summary");
        let row = text
            .lines()
            .find(|line| line.starts_with("retrieval.answers"))
            .expect("row");
        let columns = row.split_whitespace().collect::<Vec<_>>();
        // point n answered fallback agree disagree p50 p95 probes used
        assert_eq!(columns[1], "1", "probe/posthoc rows are not samples");
        assert_eq!(columns[8], "2", "two retained decisions");
        assert_eq!(columns[9], "1", "one probe matched a later use");

        let _ = std::fs::remove_dir_all(root);
    }
}
