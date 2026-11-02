use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The four retrieval branches. The order is part of the deterministic wire
/// contract and must not be changed without changing the generation version.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, Serialize, PartialOrd, Default,
)]
pub enum Branch {
    #[default]
    Exact,
    Symbol,
    Text,
    Semantic,
}

impl Branch {
    pub const DETERMINISTIC: [Self; 3] = [Self::Exact, Self::Symbol, Self::Text];
    pub const ALL: [Self; 4] = [Self::Exact, Self::Symbol, Self::Text, Self::Semantic];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Symbol => "symbol",
            Self::Text => "text",
            Self::Semantic => "semantic",
        }
    }

    pub fn ordinal(self) -> usize {
        match self {
            Self::Exact => 0,
            Self::Symbol => 1,
            Self::Text => 2,
            Self::Semantic => 3,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Default)]
pub enum BranchStatus {
    #[default]
    Ok,
    Partial {
        pending: usize,
    },
    Unavailable {
        reason: String,
    },
    Building,
    Rebuilding,
    TimedOut,
    Cancelled,
    Stale,
}

impl BranchStatus {
    pub fn is_successful(&self) -> bool {
        matches!(self, Self::Ok | Self::Partial { .. })
    }

    pub fn is_problem(&self) -> bool {
        !self.is_successful()
    }

    pub fn explanation(&self) -> Option<String> {
        match self {
            Self::Ok => None,
            Self::Partial { pending } => Some(format!("index syncing: {pending} files pending")),
            Self::Unavailable { reason } => Some(format!("unavailable ({reason})")),
            Self::Building => Some("index building — exact only".to_string()),
            Self::Rebuilding => Some("index rebuilding".to_string()),
            Self::TimedOut => Some("timed out".to_string()),
            Self::Cancelled => Some("cancelled".to_string()),
            Self::Stale => Some("stale index".to_string()),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct SearchRequest {
    pub query: String,
    pub path: Option<String>,
    pub glob: Option<String>,
    pub max_results: usize,
}

impl SearchRequest {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            path: None,
            glob: None,
            max_results: 10,
        }
    }

    pub fn bounded_max_results(&self) -> usize {
        self.max_results.clamp(1, 30)
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Candidate {
    pub branch: Branch,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub symbol: Option<String>,
    pub file_hash_raw: String,
    pub rank: usize,
    pub score: Option<f64>,
    pub branches: Vec<Branch>,
    pub dropped_reason: Option<String>,
    pub latency_ms: u64,
    pub snippet: Option<String>,
    pub no_symbol_check: bool,
    /// @nyan: `retrieval.answers` marked this candidate as answering the
    /// query. Annotation only; it never removes a candidate.
    #[serde(default)]
    pub answers_query: bool,
}

impl Candidate {
    pub fn key(&self) -> (String, usize, usize) {
        (self.path.clone(), self.start_line, self.end_line)
    }

    pub fn source_branches(&self) -> Vec<Branch> {
        if self.branches.is_empty() {
            vec![self.branch]
        } else {
            self.branches.clone()
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct BranchResult {
    pub status: BranchStatus,
    pub candidates: Vec<Candidate>,
    pub elapsed_ms: u64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct SearchOutcome {
    pub query: String,
    pub candidates: Vec<Candidate>,
    pub branches: BTreeMap<Branch, BranchResult>,
    pub dropped: Vec<Candidate>,
    pub elapsed_ms: u64,
    pub rrf_k: u32,
    pub reranked: bool,
    pub truncated: bool,
    /// @nyan: decision receipts (point, outcome, fallback reason) recorded for
    /// replay. Structured data only — never rendered to the model.
    #[serde(default)]
    pub decisions: Vec<DecisionRecord>,
    /// Whether `retrieval.sufficient` triggered the one extra round.
    #[serde(default)]
    pub extra_round: bool,
}

/// @nyan: one Decision Provider call made during a search (decision-provider
/// §3 "可复算": replays reuse these instead of re-asking).
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DecisionRecord {
    pub point: String,
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
}

impl SearchOutcome {
    pub fn branch_status(&self, branch: Branch) -> Option<&BranchStatus> {
        self.branches.get(&branch).map(|result| &result.status)
    }

    pub fn all_deterministic_succeeded(&self) -> bool {
        Branch::DETERMINISTIC.iter().all(|branch| {
            self.branch_status(*branch)
                .is_some_and(BranchStatus::is_successful)
        })
    }

    pub fn has_problem(&self) -> bool {
        self.branches
            .values()
            .any(|result| result.status.is_problem())
    }

    pub fn searched_branches(&self) -> Vec<Branch> {
        Branch::ALL
            .into_iter()
            .filter(|branch| self.branches.contains_key(branch))
            .collect()
    }
}
