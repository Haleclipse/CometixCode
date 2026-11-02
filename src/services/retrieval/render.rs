use super::outcome::{Branch, BranchStatus, SearchOutcome};

/// Render the library outcome in the compact model-facing CodeSearch format.
pub fn render_outcome(outcome: &SearchOutcome, max_results: usize) -> String {
    let shown = outcome
        .candidates
        .iter()
        .take(max_results)
        .collect::<Vec<_>>();
    let mut lines = Vec::new();
    for candidate in &shown {
        let symbol = candidate
            .symbol
            .as_deref()
            .map(|value| format!("  {value}"))
            .unwrap_or_default();
        let sources = candidate
            .source_branches()
            .into_iter()
            .map(Branch::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let symbol_check = if candidate.no_symbol_check {
            " (no symbol check)"
        } else {
            ""
        };
        let answers = if candidate.answers_query {
            " (answers query)"
        } else {
            ""
        };
        lines.push(format!(
            "{}:{}-{}{}  ({sources}){symbol_check}{answers}",
            candidate.path, candidate.start_line, candidate.end_line, symbol
        ));
        if let Some(snippet) = candidate.snippet.as_deref().filter(|text| !text.is_empty()) {
            lines.push(snippet.to_string());
        }
    }

    if lines.is_empty() {
        if outcome.all_deterministic_succeeded() && !outcome.has_problem() {
            if outcome.dropped.is_empty() {
                lines.push("No matches found".to_string());
            } else {
                lines.push(
                    "No matches found in the current worktree (all candidates failed verification)."
                        .to_string(),
                );
            }
        } else {
            let failed = outcome
                .branches
                .iter()
                .filter_map(|(branch, result)| {
                    result.status.is_problem().then_some(branch.as_str())
                })
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!(
                "No matches found in: {} (results may be incomplete)",
                if failed.is_empty() {
                    "unknown"
                } else {
                    &failed
                }
            ));
        }
    }

    let searched = outcome
        .searched_branches()
        .into_iter()
        .map(Branch::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    let mut notes = Vec::new();
    for branch in outcome.searched_branches() {
        if let Some(status) = outcome.branch_status(branch)
            && let Some(explanation) = status.explanation()
        {
            notes.push(format!("{} {explanation}", branch.as_str()));
        }
    }
    let searched_note = if notes.is_empty() {
        format!("[searched: {searched}]")
    } else {
        format!("[searched: {searched} — {}]", notes.join(", "))
    };
    lines.push(searched_note);
    if outcome.extra_round {
        lines.push(
            "[retrieval: one extra round ran because the first results were judged insufficient]"
                .to_string(),
        );
    }
    if !outcome.dropped.is_empty() {
        let mut reasons = std::collections::BTreeMap::<String, usize>::new();
        for candidate in &outcome.dropped {
            *reasons
                .entry(
                    candidate
                        .dropped_reason
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string()),
                )
                .or_default() += 1;
        }
        let detail = reasons
            .into_iter()
            .map(|(reason, count)| format!("{count} results dropped: {reason}"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("[{detail}]"));
    }

    let complete = !outcome.truncated
        && !outcome.has_problem()
        && outcome.dropped.is_empty()
        && shown.len() == outcome.candidates.len();
    if complete {
        lines.push(format!(
            "(Complete: all {} results shown.)",
            outcome.candidates.len()
        ));
    } else if shown.len() < outcome.candidates.len() {
        // More verified results exist than the caller asked to see.
        lines.push(format!(
            "(Partial: {} of {} results shown. Continue with max_results={}.)",
            shown.len(),
            outcome.candidates.len(),
            shown.len().saturating_add(1).max(1)
        ));
    } else {
        // Nothing further to page through; the shortfall is named above.
        lines.push(format!(
            "(Partial: all {} verified results shown; some branches or candidates failed — see the notes above.)",
            shown.len()
        ));
    }
    lines.join("\n")
}

pub fn status_label(status: &BranchStatus) -> &'static str {
    match status {
        BranchStatus::Ok => "ok",
        BranchStatus::Partial { .. } => "partial",
        BranchStatus::Unavailable { .. } => "unavailable",
        BranchStatus::Building => "building",
        BranchStatus::Rebuilding => "rebuilding",
        BranchStatus::TimedOut => "timed out",
        BranchStatus::Cancelled => "cancelled",
        BranchStatus::Stale => "stale",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::retrieval::outcome::{BranchResult, Candidate};
    use std::collections::BTreeMap;

    #[test]
    fn empty_success_is_distinct_from_incomplete_search() {
        let mut branches = BTreeMap::new();
        for branch in Branch::DETERMINISTIC {
            branches.insert(
                branch,
                BranchResult {
                    status: BranchStatus::Ok,
                    ..Default::default()
                },
            );
        }
        let outcome = SearchOutcome {
            branches,
            ..Default::default()
        };
        assert!(render_outcome(&outcome, 10).contains("No matches found"));

        let mut incomplete = outcome.clone();
        incomplete.branches.insert(
            Branch::Text,
            BranchResult {
                status: BranchStatus::Unavailable {
                    reason: "index down".to_string(),
                },
                candidates: Vec::new(),
                elapsed_ms: 0,
            },
        );
        assert!(render_outcome(&incomplete, 10).contains("results may be incomplete"));
    }

    #[test]
    fn candidate_render_includes_sources_and_completion_tail() {
        let candidate = Candidate {
            path: "src/lib.rs".into(),
            start_line: 2,
            end_line: 4,
            branches: vec![Branch::Exact, Branch::Text],
            ..Default::default()
        };
        let outcome = SearchOutcome {
            candidates: vec![candidate],
            branches: [(
                Branch::Exact,
                BranchResult {
                    status: BranchStatus::Ok,
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let rendered = render_outcome(&outcome, 10);
        assert!(rendered.contains("src/lib.rs:2-4"));
        assert!(rendered.contains("(exact, text)"));
        assert!(rendered.ends_with("(Complete: all 1 results shown.)"));
    }
}
