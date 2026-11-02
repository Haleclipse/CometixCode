use std::cmp::Ordering;
use std::collections::BTreeMap;

use super::outcome::{Branch, Candidate};

/// Deterministic reciprocal-rank fusion. Candidates with the same path/range
/// are merged so the model sees one verified snippet with all contributing
/// branches attached.
pub fn rrf_fuse(
    branch_candidates: impl IntoIterator<Item = (Branch, Vec<Candidate>)>,
    k: u32,
    symbol_weight: f64,
    keep: usize,
) -> Vec<Candidate> {
    let mut merged: BTreeMap<(String, usize, usize), Candidate> = BTreeMap::new();
    let mut scores: BTreeMap<(String, usize, usize), f64> = BTreeMap::new();

    for (branch, candidates) in branch_candidates {
        let weight = if branch == Branch::Symbol {
            symbol_weight
        } else {
            1.0
        };
        for (index, mut candidate) in candidates.into_iter().enumerate() {
            let rank = candidate.rank.max(index + 1);
            candidate.rank = rank;
            let key = candidate.key();
            *scores.entry(key.clone()).or_default() += weight / (k as f64 + rank as f64);
            match merged.get_mut(&key) {
                Some(existing) => {
                    if !existing.branches.contains(&branch) {
                        existing.branches.push(branch);
                    }
                    existing.branches.sort_by_key(|source| source.ordinal());
                    if existing.symbol.is_none() {
                        existing.symbol = candidate.symbol;
                    }
                    existing.no_symbol_check &= candidate.no_symbol_check;
                }
                None => {
                    candidate.branches = vec![branch];
                    candidate.score = None;
                    merged.insert(key, candidate);
                }
            }
        }
    }

    let mut results = merged
        .into_iter()
        .map(|(key, mut candidate)| {
            candidate.score = scores.get(&key).copied();
            candidate
        })
        .collect::<Vec<_>>();
    results.sort_by(compare_candidates);
    results.truncate(keep);
    results
}

pub fn compare_candidates(left: &Candidate, right: &Candidate) -> Ordering {
    right
        .score
        .unwrap_or(0.0)
        .partial_cmp(&left.score.unwrap_or(0.0))
        .unwrap_or(Ordering::Equal)
        .then_with(|| left.path.as_bytes().cmp(right.path.as_bytes()))
        .then_with(|| left.start_line.cmp(&right.start_line))
        .then_with(|| left.end_line.cmp(&right.end_line))
        .then_with(|| {
            left.source_branches()
                .first()
                .map_or(usize::MAX, |branch| branch.ordinal())
                .cmp(
                    &right
                        .source_branches()
                        .first()
                        .map_or(usize::MAX, |branch| branch.ordinal()),
                )
        })
}

pub fn symbol_weight(query: &str) -> f64 {
    let trimmed = query.trim();
    if trimmed.split_whitespace().count() == 1 && is_identifier_or_path(trimmed) {
        2.0
    } else {
        1.0
    }
}

pub fn asks_for_relations(query: &str) -> bool {
    let lower = query.to_ascii_lowercase();
    [
        "caller",
        "who calls",
        "callers",
        "calls",
        "reference",
        "usages",
        "used by",
        "dependency",
        "depend",
        "impact",
        "调用",
        "引用",
        "依赖",
        "影响",
    ]
    .iter()
    .any(|signal| lower.contains(signal))
}

fn is_identifier_or_path(value: &str) -> bool {
    value.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '/' | '.' | ':')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(branch: Branch, path: &str, rank: usize) -> Candidate {
        Candidate {
            branch,
            path: path.to_string(),
            start_line: 1,
            end_line: 2,
            rank,
            ..Candidate::default()
        }
    }

    #[test]
    fn rrf_merges_sources_and_breaks_ties_by_path() {
        let result = rrf_fuse(
            [
                (Branch::Exact, vec![candidate(Branch::Exact, "b.rs", 1)]),
                (Branch::Symbol, vec![candidate(Branch::Symbol, "a.rs", 1)]),
            ],
            60,
            1.0,
            10,
        );
        assert_eq!(result[0].path, "a.rs");
        assert_eq!(result[0].branches, vec![Branch::Symbol]);
    }

    #[test]
    fn relation_and_identifier_rules_are_deterministic() {
        assert_eq!(symbol_weight("AuthMiddleware"), 2.0);
        assert_eq!(symbol_weight("where is auth"), 1.0);
        assert!(asks_for_relations("who calls parse_config"));
        assert!(asks_for_relations("影响 parse_config"));
    }
}
