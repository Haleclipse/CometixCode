//! `SettingsJson.nyan` — the formal settings family for nyan's own modules.
//!
//! @nyan: CC has no such family. Every nyan module (compaction, cache warming,
//! memory, retrieval, web search, decision provider, tool I/O) is configured
//! under one top-level key so the settings schema stays strict about unknown
//! top-level keys (`utils/settings/validation.rs`), and so every feature has a
//! single off switch that restores upstream behaviour.
//!
//! Every field is `Option`, including inside each section. That is deliberate:
//! the settings loader merges sources by serializing each parsed source and
//! deep-merging the JSON (see `merge_settings_deep`). A section that filled in
//! its defaults at deserialize time would serialize those defaults and clobber
//! a value set by another source. With `Option` fields and
//! `skip_serializing_if`, only the keys a source actually wrote survive the
//! merge.
//!
//! Design defaults therefore live in accessors (`mask()`, `keep_recent_tokens()`
//! …), not in `Default`, which means "unset". Callers read the accessor when
//! they need the effective value.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

// ════════════════════════════════════════════════════════════
// Compaction
// ════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct CompactionSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mask: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mask_min_fraction: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_recent_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_style: Option<SummaryStyle>,
    /// `None` means "use the main model" (compaction C10).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mask_headroom: Option<f64>,
    /// Shadow mode for compaction.keep (compaction C14, decision-provider 4.4).
    /// When true, records keep-ordering vs actual-ordering comparisons to
    /// `$CLAUDE_CONFIG_DIR/decision/compaction.keep.jsonl` without altering
    /// the actual compression outcome.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_shadow: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SummaryStyle {
    Legacy,
    /// Restores the upstream Claude Code summary prompt (compaction C6).
    Cc,
}

impl CompactionSettings {
    pub fn mask(&self) -> bool {
        self.mask.unwrap_or(true)
    }
    pub fn mask_min_fraction(&self) -> f64 {
        self.mask_min_fraction.unwrap_or(0.01)
    }
    pub fn keep_recent_tokens(&self) -> u32 {
        self.keep_recent_tokens.unwrap_or(20_000)
    }
    pub fn summary_style(&self) -> SummaryStyle {
        self.summary_style.unwrap_or(SummaryStyle::Legacy)
    }
    pub fn mask_headroom(&self) -> f64 {
        self.mask_headroom.unwrap_or(0.10)
    }
    pub fn keep_shadow(&self) -> bool {
        self.keep_shadow.unwrap_or(false)
    }
}

// ════════════════════════════════════════════════════════════
// Prompt-cache warming (compaction C12)
// ════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct CacheSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warming: Option<CacheWarmingMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_cached_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_active_minutes: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_idle_minutes: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheWarmingMode {
    Off,
    Active,
    Idle,
}

impl CacheSettings {
    pub fn warming(&self) -> CacheWarmingMode {
        self.warming.unwrap_or(CacheWarmingMode::Active)
    }
    pub fn min_cached_tokens(&self) -> u32 {
        self.min_cached_tokens.unwrap_or(20_000)
    }
    pub fn max_active_minutes(&self) -> u32 {
        self.max_active_minutes.unwrap_or(60)
    }
    pub fn max_idle_minutes(&self) -> u32 {
        self.max_idle_minutes.unwrap_or(30)
    }
}

// ════════════════════════════════════════════════════════════
// Memory
// ════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct MemorySettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extract: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extract_every_turns: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extract_every_tokens: Option<u32>,
    /// `None` means "use the main model" (memory M2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extract_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extract_before_compact: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extract_before_compact_timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extract_max_turns: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consolidate: Option<ConsolidateMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consolidate_min_hours: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consolidate_min_new_files: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret_scan: Option<bool>,
    /// @nyan: per-point Decision Provider shadow switches (SMOKE-EVAL C4).
    /// Every switch is off by default; off means no request, no log, and the
    /// exact deterministic behaviour of the module.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision_shadow: Option<MemoryDecisionShadowSettings>,
}

/// @nyan: independent shadow switch per memory decision point. A point is only
/// queried when its own switch here is on **and** the Decision Provider is
/// enabled **and** the point's resolved mode in `settings.nyan.decision` is not
/// `off`. A `shadow`/`active` mode in `decision.points` alone does not turn a
/// memory point on, and even `active` is only recorded, never applied; the
/// deterministic path is never altered.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct MemoryDecisionShadowSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extract_now: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extract_skip: Option<bool>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub memory_type: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicate: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consolidate: Option<bool>,
}

impl MemoryDecisionShadowSettings {
    pub fn extract_now(&self) -> bool {
        self.extract_now.unwrap_or(false)
    }
    pub fn extract_skip(&self) -> bool {
        self.extract_skip.unwrap_or(false)
    }
    pub fn memory_type(&self) -> bool {
        self.memory_type.unwrap_or(false)
    }
    pub fn duplicate(&self) -> bool {
        self.duplicate.unwrap_or(false)
    }
    pub fn consolidate(&self) -> bool {
        self.consolidate.unwrap_or(false)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConsolidateMode {
    Auto,
    Manual,
    Off,
}

impl MemorySettings {
    pub fn extract(&self) -> bool {
        self.extract.unwrap_or(true)
    }
    pub fn extract_every_turns(&self) -> u32 {
        self.extract_every_turns.unwrap_or(8)
    }
    pub fn extract_every_tokens(&self) -> u32 {
        self.extract_every_tokens.unwrap_or(30_000)
    }
    pub fn extract_before_compact(&self) -> bool {
        self.extract_before_compact.unwrap_or(true)
    }
    pub fn extract_before_compact_timeout_ms(&self) -> u64 {
        self.extract_before_compact_timeout_ms.unwrap_or(60_000)
    }
    pub fn extract_max_turns(&self) -> u32 {
        self.extract_max_turns.unwrap_or(6)
    }
    pub fn consolidate(&self) -> ConsolidateMode {
        self.consolidate.unwrap_or(ConsolidateMode::Auto)
    }
    pub fn consolidate_min_hours(&self) -> u32 {
        self.consolidate_min_hours.unwrap_or(24)
    }
    pub fn consolidate_min_new_files(&self) -> u32 {
        self.consolidate_min_new_files.unwrap_or(5)
    }
    pub fn secret_scan(&self) -> bool {
        self.secret_scan.unwrap_or(true)
    }
    pub fn decision_shadow(&self) -> MemoryDecisionShadowSettings {
        self.decision_shadow.clone().unwrap_or_default()
    }
}

// ════════════════════════════════════════════════════════════
// Retrieval
// ════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct RetrievalSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<RetrievalIndexSettings>,
    /// Absent means the semantic branch is not configured and is never called.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic: Option<SemanticSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rerank: Option<RerankSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<RetrievalBudgetSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync: Option<RetrievalSyncSettings>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct RetrievalIndexSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_file_bytes: Option<u64>,
    /// `"auto"` or an explicit language list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub languages: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct SemanticSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedding: Option<EmbeddingSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qdrant: Option<QdrantSettings>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct EmbeddingSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dim: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_file: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct QdrantSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_file: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct RerankSettings {
    /// Code retrieval switch. The web rerank switch is independent
    /// (`nyan.web.search.rerank.enabled`, external search 3.3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_n: Option<u32>,
    /// Characters of each document sent to the reranker. The caller's snippet
    /// is not shortened. Default stays inside the service's 4096-token limit
    /// at one token per character, with room for the template and the query.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_document_chars: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct RetrievalBudgetSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_deadline_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rrf_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connect_timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embed_timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qdrant_timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rerank_timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct RetrievalSyncSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_size: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embed_batch_timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qdrant_batch_timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_batch_failures: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prune_after_days: Option<u32>,
}

impl RetrievalSettings {
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
    pub fn index(&self) -> RetrievalIndexSettings {
        self.index.clone().unwrap_or_default()
    }
    /// `Some` means the semantic branch is configured. An empty section still
    /// counts as configured: the user opted in and the defaults apply.
    pub fn semantic(&self) -> Option<SemanticSettings> {
        self.semantic.clone()
    }
    pub fn rerank(&self) -> RerankSettings {
        self.rerank.clone().unwrap_or_default()
    }
    pub fn budget(&self) -> RetrievalBudgetSettings {
        self.budget.clone().unwrap_or_default()
    }
    pub fn sync(&self) -> RetrievalSyncSettings {
        self.sync.clone().unwrap_or_default()
    }
}

impl RetrievalIndexSettings {
    pub fn max_file_bytes(&self) -> u64 {
        self.max_file_bytes.unwrap_or(1_048_576)
    }
    pub fn languages(&self) -> &str {
        self.languages.as_deref().unwrap_or("auto")
    }
}

impl SemanticSettings {
    pub fn embedding(&self) -> EmbeddingSettings {
        self.embedding.clone().unwrap_or_default()
    }
    pub fn qdrant(&self) -> QdrantSettings {
        self.qdrant.clone().unwrap_or_default()
    }
}

impl EmbeddingSettings {
    pub fn url(&self) -> &str {
        self.url.as_deref().unwrap_or("http://10.144.0.11:8090")
    }
    pub fn model(&self) -> &str {
        self.model.as_deref().unwrap_or("jina-code-embeddings-1.5b")
    }
    pub fn dim(&self) -> u32 {
        self.dim.unwrap_or(1536)
    }
}

impl QdrantSettings {
    pub fn url(&self) -> &str {
        self.url.as_deref().unwrap_or("http://10.144.0.3:6333")
    }
    pub fn api_key_file(&self) -> Option<&str> {
        self.api_key_file.as_deref().or(Some("~/.nyan/keys/qdrant"))
    }
}

impl RerankSettings {
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(false)
    }
    pub fn url(&self) -> &str {
        self.url.as_deref().unwrap_or("http://10.144.0.11:8091")
    }
    pub fn model(&self) -> &str {
        self.model.as_deref().unwrap_or("qwen3-reranker-0.6b")
    }
    pub fn top_n(&self) -> u32 {
        // Eval 2026-11-01: top_n=10 beat 20 on Hit@5 and MRR at about 1/4 the latency.
        self.top_n.unwrap_or(10)
    }
    pub fn max_document_chars(&self) -> u32 {
        self.max_document_chars.unwrap_or(3_000)
    }
}

impl RetrievalBudgetSettings {
    pub fn deadline_ms(&self) -> u64 {
        self.deadline_ms.unwrap_or(8_000)
    }
    pub fn semantic_deadline_ms(&self) -> u64 {
        self.semantic_deadline_ms.unwrap_or(4_500)
    }
    pub fn rrf_k(&self) -> u32 {
        self.rrf_k.unwrap_or(60)
    }
    pub fn connect_timeout_ms(&self) -> u64 {
        self.connect_timeout_ms.unwrap_or(1_500)
    }
    pub fn embed_timeout_ms(&self) -> u64 {
        self.embed_timeout_ms.unwrap_or(2_500)
    }
    pub fn qdrant_timeout_ms(&self) -> u64 {
        self.qdrant_timeout_ms.unwrap_or(1_500)
    }
    pub fn rerank_timeout_ms(&self) -> u64 {
        self.rerank_timeout_ms.unwrap_or(2_000)
    }
}

impl RetrievalSyncSettings {
    pub fn batch_size(&self) -> u32 {
        self.batch_size.unwrap_or(128)
    }
    /// Files in flight during sync. Each one is an embed→upsert pipeline that
    /// waits ~0.7 s on the Qdrant round trip from nyan-pc, so the window has
    /// to be wide enough to hide that RTT.
    pub fn concurrency(&self) -> u32 {
        self.concurrency.unwrap_or(8)
    }
    pub fn embed_batch_timeout_ms(&self) -> u64 {
        self.embed_batch_timeout_ms.unwrap_or(60_000)
    }
    /// Sync-batch Qdrant timeout. The query path keeps `budget.qdrantTimeoutMs`
    /// (1.5 s); a 128-point upsert to nyan-ops measured 0.8–2.1 s, so the
    /// query-side cap would abort healthy batches.
    pub fn qdrant_batch_timeout_ms(&self) -> u64 {
        self.qdrant_batch_timeout_ms.unwrap_or(15_000)
    }
    pub fn max_batch_failures(&self) -> u32 {
        self.max_batch_failures.unwrap_or(3)
    }
    pub fn prune_after_days(&self) -> u32 {
        self.prune_after_days.unwrap_or(90)
    }
}

// ════════════════════════════════════════════════════════════
// Web (external search)
// ════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct WebSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub providers: Option<BTreeMap<String, ProviderConfig>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search: Option<WebSearchSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetch: Option<WebFetchSettings>,
    /// Provider fallback order for WebFetch (external search 4.6).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetch_fallback: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetch_via: Option<FetchVia>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_hosts: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deny_hosts: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_ports: Option<Vec<u16>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    /// Unicode scalars (external search 4.4); hard cap 40000.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_return_chars: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_retention_days: Option<u32>,
    /// `None` means "use the main model".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extract_model: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct ProviderConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_file: Option<String>,
    /// Test/mocking seam; absent means the provider's official URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// SearXNG only: the deployed instance URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_calls_per_session: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct WebSearchSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recall_depth: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fusion_keep: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain_cap: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rrf_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_provider_deadline_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_per_session: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_concurrent_searches: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rerank: Option<WebRerankSettings>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct WebRerankSettings {
    /// Independent of `nyan.retrieval.rerank.enabled` (external search 3.3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet_chars: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct WebFetchSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connect_timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_redirects: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_per_session: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_bytes_per_session: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_deadline_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FetchVia {
    Proxy,
    Direct,
}

/// Built-in provider defaults from external-search 2.2 / 5.
pub struct ProviderDefaults {
    pub name: &'static str,
    pub api_key_file: Option<&'static str>,
    pub max_calls_per_session: Option<u32>,
    pub url: Option<&'static str>,
}

pub const BUILTIN_PROVIDERS: &[ProviderDefaults] = &[
    ProviderDefaults {
        name: "anysearch",
        api_key_file: Some("~/.nyan/keys/anysearch"),
        max_calls_per_session: Some(40),
        url: None,
    },
    ProviderDefaults {
        name: "brave",
        api_key_file: Some("~/.nyan/keys/brave"),
        max_calls_per_session: Some(40),
        url: None,
    },
    ProviderDefaults {
        name: "deepseek",
        api_key_file: Some("~/.nyan/keys/deepseek"),
        max_calls_per_session: Some(60),
        url: None,
    },
    ProviderDefaults {
        name: "exa",
        api_key_file: Some("~/.nyan/keys/exa"),
        max_calls_per_session: Some(30),
        url: None,
    },
    ProviderDefaults {
        name: "firecrawl",
        api_key_file: Some("~/.nyan/keys/firecrawl"),
        max_calls_per_session: Some(20),
        url: None,
    },
    ProviderDefaults {
        name: "tavily",
        api_key_file: Some("~/.nyan/keys/tavily"),
        max_calls_per_session: Some(20),
        url: None,
    },
    ProviderDefaults {
        name: "searxng",
        api_key_file: None,
        max_calls_per_session: None,
        url: Some("http://10.144.0.3:8454"),
    },
];

pub fn provider_names() -> Vec<&'static str> {
    BUILTIN_PROVIDERS.iter().map(|p| p.name).collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedProvider {
    pub enabled: bool,
    pub api_key_file: Option<String>,
    pub base_url: Option<String>,
    pub url: Option<String>,
    pub max_calls_per_session: Option<u32>,
}

impl WebSettings {
    pub fn provider(&self, name: &str) -> ResolvedProvider {
        let builtin = BUILTIN_PROVIDERS.iter().find(|p| p.name == name);
        let config = self
            .providers
            .as_ref()
            .and_then(|providers| providers.get(name));
        ResolvedProvider {
            enabled: config.and_then(|c| c.enabled).unwrap_or(true),
            api_key_file: config
                .and_then(|c| c.api_key_file.clone())
                .or_else(|| builtin.and_then(|b| b.api_key_file.map(str::to_owned))),
            base_url: config.and_then(|c| c.base_url.clone()),
            url: config
                .and_then(|c| c.url.clone())
                .or_else(|| builtin.and_then(|b| b.url.map(str::to_owned))),
            max_calls_per_session: config
                .and_then(|c| c.max_calls_per_session)
                .or_else(|| builtin.and_then(|b| b.max_calls_per_session)),
        }
    }

    pub fn search(&self) -> WebSearchSettings {
        self.search.clone().unwrap_or_default()
    }
    pub fn fetch(&self) -> WebFetchSettings {
        self.fetch.clone().unwrap_or_default()
    }
    pub fn fetch_via(&self) -> FetchVia {
        self.fetch_via.unwrap_or(FetchVia::Proxy)
    }
    pub fn fallback_order(&self) -> Vec<String> {
        self.fetch_fallback.clone().unwrap_or_else(|| {
            ["firecrawl", "anysearch", "exa", "deepseek", "tavily"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        })
    }
    pub fn allow_hosts(&self) -> &[String] {
        self.allow_hosts.as_deref().unwrap_or(&[])
    }
    pub fn deny_hosts(&self) -> &[String] {
        self.deny_hosts.as_deref().unwrap_or(&[])
    }
    pub fn allow_ports(&self) -> &[u16] {
        self.allow_ports.as_deref().unwrap_or(&[])
    }
    pub fn max_bytes(&self) -> u64 {
        self.max_bytes.unwrap_or(5_242_880)
    }
    pub fn max_return_chars(&self) -> u32 {
        self.max_return_chars.unwrap_or(20_000).min(40_000)
    }
    pub fn snapshot_retention_days(&self) -> u32 {
        self.snapshot_retention_days.unwrap_or(30)
    }
}

impl WebSearchSettings {
    pub fn recall_depth(&self) -> u32 {
        self.recall_depth.unwrap_or(20)
    }
    pub fn fusion_keep(&self) -> u32 {
        self.fusion_keep.unwrap_or(30)
    }
    pub fn domain_cap(&self) -> u32 {
        self.domain_cap.unwrap_or(3)
    }
    pub fn rrf_k(&self) -> u32 {
        self.rrf_k.unwrap_or(60)
    }
    pub fn per_provider_deadline_ms(&self) -> u64 {
        self.per_provider_deadline_ms.unwrap_or(8_000)
    }
    pub fn deadline_ms(&self) -> u64 {
        self.deadline_ms.unwrap_or(10_000)
    }
    pub fn max_per_session(&self) -> u32 {
        self.max_per_session.unwrap_or(40)
    }
    pub fn max_concurrent_searches(&self) -> u32 {
        self.max_concurrent_searches.unwrap_or(2)
    }
    pub fn rerank(&self) -> WebRerankSettings {
        self.rerank.clone().unwrap_or_default()
    }
}

impl WebRerankSettings {
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms.unwrap_or(2_000)
    }
    pub fn snippet_chars(&self) -> u32 {
        self.snippet_chars.unwrap_or(500)
    }
}

impl WebFetchSettings {
    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms.unwrap_or(10_000)
    }
    pub fn connect_timeout_ms(&self) -> u64 {
        self.connect_timeout_ms.unwrap_or(5_000)
    }
    pub fn max_redirects(&self) -> u32 {
        self.max_redirects.unwrap_or(5)
    }
    pub fn max_per_session(&self) -> u32 {
        self.max_per_session.unwrap_or(60)
    }
    pub fn max_bytes_per_session(&self) -> u64 {
        self.max_bytes_per_session.unwrap_or(104_857_600)
    }
    pub fn fallback_deadline_ms(&self) -> u64 {
        self.fallback_deadline_ms.unwrap_or(30_000)
    }
}

// ════════════════════════════════════════════════════════════
// Decision Provider
// ════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct DecisionSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emrank: Option<EmrankSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat: Option<ChatBackendSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<DecisionDeadlineSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_batch: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_calls_per_session: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_input_tokens_per_session: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub points: Option<BTreeMap<String, DecisionPointConfig>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_retention_days: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct EmrankSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_file: Option<String>,
    /// `fast` / `accurate` → model ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_idle_timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prewarm: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct ChatBackendSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct DecisionDeadlineSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub critical: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct DecisionPointConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<DecisionMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<DecisionModel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_confidence: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_prob: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_at_least: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecisionMode {
    Off,
    Shadow,
    Active,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecisionModel {
    Fast,
    Accurate,
}

/// Built-in mode/model/threshold per decision point (decision-provider 4 /
/// 8). The table is the source of truth for both the settings schema's key
/// set and the decision module's closed enum.
pub struct BuiltinDecisionPoint {
    pub name: &'static str,
    pub mode: DecisionMode,
    pub model: DecisionModel,
    pub min_confidence: f64,
    pub max_prob: Option<f64>,
    pub keep_at_least: Option<u32>,
}

pub const BUILTIN_DECISION_POINTS: &[BuiltinDecisionPoint] = &[
    BuiltinDecisionPoint {
        name: "retrieval.intent",
        mode: DecisionMode::Active,
        model: DecisionModel::Fast,
        min_confidence: 0.7,
        max_prob: None,
        keep_at_least: None,
    },
    BuiltinDecisionPoint {
        name: "retrieval.answers",
        mode: DecisionMode::Active,
        model: DecisionModel::Fast,
        min_confidence: 0.6,
        max_prob: None,
        keep_at_least: None,
    },
    BuiltinDecisionPoint {
        name: "retrieval.sufficient",
        mode: DecisionMode::Active,
        model: DecisionModel::Fast,
        min_confidence: 0.7,
        max_prob: None,
        keep_at_least: None,
    },
    BuiltinDecisionPoint {
        name: "retrieval.prune",
        mode: DecisionMode::Shadow,
        model: DecisionModel::Fast,
        min_confidence: 0.7,
        max_prob: Some(0.05),
        keep_at_least: Some(3),
    },
    BuiltinDecisionPoint {
        name: "web.answers",
        mode: DecisionMode::Active,
        model: DecisionModel::Fast,
        min_confidence: 0.6,
        max_prob: None,
        keep_at_least: None,
    },
    BuiltinDecisionPoint {
        name: "web.page_answers",
        mode: DecisionMode::Active,
        model: DecisionModel::Fast,
        min_confidence: 0.7,
        max_prob: None,
        keep_at_least: None,
    },
    BuiltinDecisionPoint {
        name: "web.fetch_fallback",
        mode: DecisionMode::Active,
        model: DecisionModel::Accurate,
        min_confidence: 0.8,
        max_prob: None,
        keep_at_least: None,
    },
    BuiltinDecisionPoint {
        name: "web.query_kind",
        mode: DecisionMode::Active,
        model: DecisionModel::Fast,
        min_confidence: 0.7,
        max_prob: None,
        keep_at_least: None,
    },
    BuiltinDecisionPoint {
        name: "web.prune",
        mode: DecisionMode::Shadow,
        model: DecisionModel::Fast,
        min_confidence: 0.7,
        max_prob: Some(0.05),
        keep_at_least: Some(5),
    },
    BuiltinDecisionPoint {
        name: "memory.extract_now",
        mode: DecisionMode::Active,
        model: DecisionModel::Accurate,
        min_confidence: 0.8,
        max_prob: None,
        keep_at_least: None,
    },
    BuiltinDecisionPoint {
        name: "memory.extract_skip",
        mode: DecisionMode::Shadow,
        model: DecisionModel::Accurate,
        min_confidence: 0.8,
        max_prob: None,
        keep_at_least: None,
    },
    BuiltinDecisionPoint {
        name: "memory.type",
        mode: DecisionMode::Active,
        model: DecisionModel::Accurate,
        min_confidence: 0.7,
        max_prob: None,
        keep_at_least: None,
    },
    BuiltinDecisionPoint {
        name: "memory.duplicate",
        mode: DecisionMode::Active,
        model: DecisionModel::Accurate,
        min_confidence: 0.7,
        max_prob: None,
        keep_at_least: None,
    },
    BuiltinDecisionPoint {
        name: "memory.consolidate",
        mode: DecisionMode::Shadow,
        model: DecisionModel::Accurate,
        min_confidence: 0.7,
        max_prob: None,
        keep_at_least: None,
    },
    BuiltinDecisionPoint {
        name: "compaction.keep",
        mode: DecisionMode::Shadow,
        model: DecisionModel::Fast,
        min_confidence: 0.7,
        max_prob: None,
        keep_at_least: None,
    },
    BuiltinDecisionPoint {
        name: "permissions.yolo_stage1",
        mode: DecisionMode::Shadow,
        model: DecisionModel::Fast,
        min_confidence: 0.9,
        max_prob: None,
        keep_at_least: None,
    },
    BuiltinDecisionPoint {
        name: "permissions.explain_risk",
        mode: DecisionMode::Shadow,
        model: DecisionModel::Accurate,
        min_confidence: 0.8,
        max_prob: None,
        keep_at_least: None,
    },
    BuiltinDecisionPoint {
        name: "prompt_suggestion.generate_gate",
        mode: DecisionMode::Shadow,
        model: DecisionModel::Fast,
        min_confidence: 0.75,
        max_prob: None,
        keep_at_least: None,
    },
    BuiltinDecisionPoint {
        name: "speculation.start",
        mode: DecisionMode::Shadow,
        model: DecisionModel::Fast,
        min_confidence: 0.8,
        max_prob: None,
        keep_at_least: None,
    },
    BuiltinDecisionPoint {
        name: "context_suggestions.rank",
        mode: DecisionMode::Shadow,
        model: DecisionModel::Fast,
        min_confidence: 0.7,
        max_prob: None,
        keep_at_least: None,
    },
    BuiltinDecisionPoint {
        name: "agent.route",
        mode: DecisionMode::Off,
        model: DecisionModel::Accurate,
        min_confidence: 0.85,
        max_prob: None,
        keep_at_least: None,
    },
    BuiltinDecisionPoint {
        name: "plan.verify_need",
        mode: DecisionMode::Shadow,
        model: DecisionModel::Accurate,
        min_confidence: 0.8,
        max_prob: None,
        keep_at_least: None,
    },
    BuiltinDecisionPoint {
        name: "skill.rank",
        mode: DecisionMode::Off,
        model: DecisionModel::Fast,
        min_confidence: 0.8,
        max_prob: None,
        keep_at_least: None,
    },
    BuiltinDecisionPoint {
        name: "todo.rank",
        mode: DecisionMode::Off,
        model: DecisionModel::Fast,
        min_confidence: 0.7,
        max_prob: None,
        keep_at_least: None,
    },
];

pub fn decision_point_names() -> Vec<&'static str> {
    BUILTIN_DECISION_POINTS.iter().map(|p| p.name).collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedDecisionPoint {
    pub name: String,
    pub mode: DecisionMode,
    pub model: DecisionModel,
    pub min_confidence: f64,
    pub max_prob: Option<f64>,
    pub keep_at_least: Option<u32>,
}

impl DecisionSettings {
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
    pub fn emrank(&self) -> EmrankSettings {
        self.emrank.clone().unwrap_or_default()
    }
    pub fn max_batch(&self) -> u32 {
        self.max_batch.unwrap_or(30)
    }
    pub fn max_calls_per_session(&self) -> u32 {
        self.max_calls_per_session.unwrap_or(600)
    }
    pub fn max_input_tokens_per_session(&self) -> u64 {
        self.max_input_tokens_per_session.unwrap_or(3_000_000)
    }
    pub fn log_retention_days(&self) -> u32 {
        self.log_retention_days.unwrap_or(30)
    }
    pub fn deadline(&self, critical: bool) -> std::time::Duration {
        let deadlines = self.deadline_ms.clone().unwrap_or_default();
        let millis = if critical {
            deadlines.critical.unwrap_or(800)
        } else {
            deadlines.background.unwrap_or(2_000)
        };
        std::time::Duration::from_millis(millis)
    }
    /// Built-in defaults overlaid with the user's per-point config.
    pub fn resolved_point(&self, name: &str) -> Option<ResolvedDecisionPoint> {
        let builtin = BUILTIN_DECISION_POINTS.iter().find(|p| p.name == name)?;
        let config = self.points.as_ref().and_then(|points| points.get(name));
        Some(ResolvedDecisionPoint {
            name: name.to_string(),
            mode: config.and_then(|c| c.mode).unwrap_or(builtin.mode),
            model: config.and_then(|c| c.model).unwrap_or(builtin.model),
            min_confidence: config
                .and_then(|c| c.min_confidence)
                .unwrap_or(builtin.min_confidence),
            max_prob: config.and_then(|c| c.max_prob).or(builtin.max_prob),
            keep_at_least: config
                .and_then(|c| c.keep_at_least)
                .or(builtin.keep_at_least),
        })
    }
    /// Retrieval / web / compaction points sit on the critical path; memory,
    /// permissions and the host-runtime points are background (decision-provider 5.2).
    pub fn is_critical_point(name: &str) -> bool {
        name.starts_with("retrieval.")
            || name.starts_with("web.")
            || name.starts_with("compaction.")
    }
}

impl EmrankSettings {
    pub fn url(&self) -> &str {
        self.url
            .as_deref()
            .unwrap_or("https://emrank.tech/v1/decisions")
    }
    pub fn api_key_file(&self) -> &str {
        self.api_key_file
            .as_deref()
            .unwrap_or("~/.nyan/keys/emrank")
    }
    pub fn model_id(&self, alias: DecisionModel) -> String {
        let fallback = match alias {
            DecisionModel::Fast => "typesafe/jev-1.14.0",
            DecisionModel::Accurate => "cloudflare/clef",
        };
        let key = match alias {
            DecisionModel::Fast => "fast",
            DecisionModel::Accurate => "accurate",
        };
        self.models
            .as_ref()
            .and_then(|models| models.get(key))
            .cloned()
            .unwrap_or_else(|| fallback.to_string())
    }
    pub fn pool_idle_timeout_ms(&self) -> u64 {
        self.pool_idle_timeout_ms.unwrap_or(90_000)
    }
    pub fn prewarm(&self) -> bool {
        self.prewarm.unwrap_or(true)
    }
}

// ════════════════════════════════════════════════════════════
// Tool I/O (tool-io.md)
// ════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct ToolIoSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_notes: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stable_grep_order: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_partial_on_overflow: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_batch_max: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding_detection: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_binary_check: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_line_chars: Option<MaxLineCharsSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bash_head_tail: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report_unreachable: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct MaxLineCharsSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grep: Option<u32>,
}

impl ToolIoSettings {
    pub fn completion_notes(&self) -> bool {
        self.completion_notes.unwrap_or(true)
    }
    pub fn stable_grep_order(&self) -> bool {
        self.stable_grep_order.unwrap_or(true)
    }
    pub fn read_partial_on_overflow(&self) -> bool {
        self.read_partial_on_overflow.unwrap_or(true)
    }
    /// 0 disables batch reads.
    pub fn read_batch_max(&self) -> u32 {
        self.read_batch_max.unwrap_or(16)
    }
    pub fn encoding_detection(&self) -> bool {
        self.encoding_detection.unwrap_or(true)
    }
    pub fn content_binary_check(&self) -> bool {
        self.content_binary_check.unwrap_or(true)
    }
    pub fn max_line_chars(&self) -> MaxLineCharsSettings {
        self.max_line_chars.clone().unwrap_or_default()
    }
    pub fn bash_head_tail(&self) -> bool {
        self.bash_head_tail.unwrap_or(true)
    }
    pub fn report_unreachable(&self) -> bool {
        self.report_unreachable.unwrap_or(true)
    }
}

impl MaxLineCharsSettings {
    pub fn read(&self) -> u32 {
        self.read.unwrap_or(2_000)
    }
    pub fn grep(&self) -> u32 {
        self.grep.unwrap_or(500)
    }
}

// ════════════════════════════════════════════════════════════
// Provider / model profiles
// ════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Anthropic,
    Openai,
    Gemini,
    Vllm,
    Ollama,
    Openrouter,
    Gateway,
}

impl ProviderKind {
    /// Commercial classification is fixed by the built-in provider kind.
    pub fn is_commercial(self) -> bool {
        matches!(
            self,
            Self::Anthropic | Self::Openai | Self::Gemini | Self::Openrouter | Self::Gateway
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderProtocol {
    AnthropicMessages,
    OpenaiChat,
    OpenaiResponses,
    Gemini,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct CredentialReference {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub optional_env: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub none: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct ProviderSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<ProviderKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<ProviderProtocol>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocols: Option<Vec<ProviderProtocol>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<CredentialReference>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct ModelCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub streaming: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct ModelProfileSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<ModelCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<ModelCostSettings>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct ModelCostSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct SelectionDecisionSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fast: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accurate: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct SelectionSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main_fallbacks: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagents: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<SelectionDecisionSettings>,
}

// ════════════════════════════════════════════════════════════
// Root
// ════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct NyanSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub providers: Option<BTreeMap<String, ProviderSettings>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<BTreeMap<String, ModelProfileSettings>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection: Option<SelectionSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compaction: Option<CompactionSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache: Option<CacheSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemorySettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retrieval: Option<RetrievalSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web: Option<WebSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<DecisionSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_io: Option<ToolIoSettings>,
}

impl NyanSettings {
    pub fn compaction(&self) -> CompactionSettings {
        self.compaction.clone().unwrap_or_default()
    }
    pub fn cache(&self) -> CacheSettings {
        self.cache.clone().unwrap_or_default()
    }
    pub fn memory(&self) -> MemorySettings {
        self.memory.clone().unwrap_or_default()
    }
    pub fn retrieval(&self) -> RetrievalSettings {
        self.retrieval.clone().unwrap_or_default()
    }
    pub fn web(&self) -> WebSettings {
        self.web.clone().unwrap_or_default()
    }
    pub fn decision(&self) -> DecisionSettings {
        self.decision.clone().unwrap_or_default()
    }
    pub fn tool_io(&self) -> ToolIoSettings {
        self.tool_io.clone().unwrap_or_default()
    }

    /// Code-retrieval rerank switch (default off, retrieval 4.9).
    pub fn retrieval_rerank_enabled(&self) -> bool {
        self.retrieval().rerank().enabled()
    }
    /// Web-search rerank switch (default on, external search 3.3). It does not
    /// depend on `nyan.retrieval.enabled` or the code-retrieval switch.
    pub fn web_rerank_enabled(&self) -> bool {
        self.web().search().rerank().enabled()
    }
}

// ════════════════════════════════════════════════════════════
// Zod schema
// ════════════════════════════════════════════════════════════

/// Maps to no CC schema: the formal carrier for `SettingsJson.nyan`.
///
/// Nested objects use `zod::object` (unknown keys stripped), matching every
/// other settings family. Top-level strictness is applied by
/// `validate_settings_file_content`, so an unknown key beside `nyan` still
/// fails.
pub fn nyan_settings_schema() -> crate::utils::zod::Schema {
    use crate::utils::zod;

    let credential = zod::union(vec![
        zod::strict_object(vec![("env", zod::string().min(1))]),
        zod::strict_object(vec![("file", zod::string().min(1))]),
        zod::strict_object(vec![("optionalEnv", zod::string().min(1))]),
        zod::strict_object(vec![("oauth", zod::string().min(1))]),
        zod::strict_object(vec![("none", zod::literal(serde_json::json!(true)))]),
    ]);
    let provider = zod::strict_object(vec![
        (
            "kind",
            zod::enumeration(vec![
                "anthropic",
                "openai",
                "gemini",
                "vllm",
                "ollama",
                "openrouter",
                "gateway",
            ]),
        ),
        (
            "protocol",
            zod::enumeration(vec![
                "anthropicMessages",
                "openaiChat",
                "openaiResponses",
                "gemini",
            ])
            .optional(),
        ),
        (
            "protocols",
            zod::array(zod::enumeration(vec![
                "anthropicMessages",
                "openaiChat",
                "openaiResponses",
                "gemini",
            ]))
            .optional(),
        ),
        ("baseUrl", zod::string().min(1).optional()),
        ("credential", credential.optional()),
    ]);
    let capabilities = zod::strict_object(vec![
        ("contextWindow", zod::number().int().positive().optional()),
        ("maxOutputTokens", zod::number().int().positive().optional()),
        ("tools", zod::enumeration(vec!["none", "native"]).optional()),
        (
            "thinking",
            zod::enumeration(vec!["none", "adaptive", "budgeted"]).optional(),
        ),
        ("images", zod::enumeration(vec!["none", "input"]).optional()),
        ("structuredOutput", zod::boolean().optional()),
        ("streaming", zod::boolean().optional()),
        (
            "promptCache",
            zod::enumeration(vec!["none", "native"]).optional(),
        ),
    ]);
    let cost = zod::strict_object(vec![
        ("input", zod::number().min(0).optional()),
        ("output", zod::number().min(0).optional()),
        ("cacheRead", zod::number().min(0).optional()),
        ("cacheWrite", zod::number().min(0).optional()),
    ]);
    let model = zod::strict_object(vec![
        ("provider", zod::string().min(1)),
        ("model", zod::string().min(1)),
        ("displayName", zod::string().min(1).optional()),
        ("capabilities", capabilities.optional()),
        (
            "input",
            zod::array(zod::enumeration(vec!["text", "image"])).optional(),
        ),
        ("cost", cost.optional()),
    ]);
    let decision_selection = zod::strict_object(vec![
        ("fast", zod::string().min(1).optional()),
        ("accurate", zod::string().min(1).optional()),
    ]);
    let selection = zod::strict_object(vec![
        ("main", zod::string().min(1).optional()),
        ("mainFallbacks", zod::array(zod::string().min(1)).optional()),
        ("subagent", zod::string().min(1).optional()),
        ("subagents", zod::record(zod::string().min(1)).optional()),
        ("decision", decision_selection.optional()),
    ]);

    let compaction = zod::object(vec![
        ("mask", zod::boolean().optional()),
        ("maskMinFraction", zod::number().min(0).max(1).optional()),
        (
            "keepRecentTokens",
            zod::number().int().nonnegative().optional(),
        ),
        (
            "summaryStyle",
            zod::enumeration(vec!["legacy", "cc"]).optional(),
        ),
        ("model", zod::string().optional()),
        ("maskHeadroom", zod::number().min(0).max(1).optional()),
        ("keepShadow", zod::boolean().optional()),
    ]);

    let cache = zod::object(vec![
        (
            "warming",
            zod::enumeration(vec!["off", "active", "idle"]).optional(),
        ),
        (
            "minCachedTokens",
            zod::number().int().nonnegative().optional(),
        ),
        (
            "maxActiveMinutes",
            zod::number().int().nonnegative().optional(),
        ),
        (
            "maxIdleMinutes",
            zod::number().int().nonnegative().optional(),
        ),
    ]);

    let memory = zod::object(vec![
        ("extract", zod::boolean().optional()),
        (
            "extractEveryTurns",
            zod::number().int().positive().optional(),
        ),
        (
            "extractEveryTokens",
            zod::number().int().positive().optional(),
        ),
        ("extractModel", zod::string().optional()),
        ("extractBeforeCompact", zod::boolean().optional()),
        (
            "extractBeforeCompactTimeoutMs",
            zod::number().int().positive().optional(),
        ),
        ("extractMaxTurns", zod::number().int().positive().optional()),
        (
            "consolidate",
            zod::enumeration(vec!["auto", "manual", "off"]).optional(),
        ),
        (
            "consolidateMinHours",
            zod::number().int().nonnegative().optional(),
        ),
        (
            "consolidateMinNewFiles",
            zod::number().int().nonnegative().optional(),
        ),
        ("secretScan", zod::boolean().optional()),
        (
            "decisionShadow",
            zod::object(vec![
                ("extractNow", zod::boolean().optional()),
                ("extractSkip", zod::boolean().optional()),
                ("type", zod::boolean().optional()),
                ("duplicate", zod::boolean().optional()),
                ("consolidate", zod::boolean().optional()),
            ])
            .optional(),
        ),
    ]);

    let retrieval_index = zod::object(vec![
        ("maxFileBytes", zod::number().int().positive().optional()),
        ("languages", zod::string().optional()),
    ]);

    let embedding = zod::object(vec![
        ("url", zod::string().optional()),
        ("model", zod::string().optional()),
        ("dim", zod::number().int().positive().optional()),
        ("apiKeyFile", zod::string().optional()),
    ]);
    let qdrant = zod::object(vec![
        ("url", zod::string().optional()),
        ("apiKeyFile", zod::string().optional()),
    ]);
    let semantic = zod::object(vec![
        ("embedding", embedding.optional()),
        ("qdrant", qdrant.optional()),
    ]);

    let rerank = zod::object(vec![
        ("enabled", zod::boolean().optional()),
        ("url", zod::string().optional()),
        ("model", zod::string().optional()),
        ("topN", zod::number().int().positive().optional()),
        (
            "maxDocumentChars",
            zod::number().int().positive().optional(),
        ),
    ]);
    let retrieval_budget = zod::object(vec![
        ("deadlineMs", zod::number().int().positive().optional()),
        (
            "semanticDeadlineMs",
            zod::number().int().positive().optional(),
        ),
        ("rrfK", zod::number().int().positive().optional()),
        (
            "connectTimeoutMs",
            zod::number().int().positive().optional(),
        ),
        ("embedTimeoutMs", zod::number().int().positive().optional()),
        ("qdrantTimeoutMs", zod::number().int().positive().optional()),
        ("rerankTimeoutMs", zod::number().int().positive().optional()),
    ]);
    let retrieval_sync = zod::object(vec![
        ("batchSize", zod::number().int().positive().optional()),
        ("concurrency", zod::number().int().positive().optional()),
        (
            "embedBatchTimeoutMs",
            zod::number().int().positive().optional(),
        ),
        (
            "qdrantBatchTimeoutMs",
            zod::number().int().positive().optional(),
        ),
        (
            "maxBatchFailures",
            zod::number().int().nonnegative().optional(),
        ),
        (
            "pruneAfterDays",
            zod::number().int().nonnegative().optional(),
        ),
    ]);
    let retrieval = zod::object(vec![
        ("enabled", zod::boolean().optional()),
        ("index", retrieval_index.optional()),
        ("semantic", semantic.optional()),
        ("rerank", rerank.optional()),
        ("budget", retrieval_budget.optional()),
        ("sync", retrieval_sync.optional()),
    ]);

    let web_provider = zod::object(vec![
        ("enabled", zod::boolean().optional()),
        ("apiKeyFile", zod::string().optional()),
        ("baseUrl", zod::string().optional()),
        ("url", zod::string().optional()),
        (
            "maxCallsPerSession",
            zod::number().int().nonnegative().optional(),
        ),
    ]);
    let web_rerank = zod::object(vec![
        ("enabled", zod::boolean().optional()),
        ("timeoutMs", zod::number().int().positive().optional()),
        ("snippetChars", zod::number().int().positive().optional()),
    ]);
    let web_search = zod::object(vec![
        ("recallDepth", zod::number().int().positive().optional()),
        ("fusionKeep", zod::number().int().positive().optional()),
        ("domainCap", zod::number().int().positive().optional()),
        ("rrfK", zod::number().int().positive().optional()),
        (
            "perProviderDeadlineMs",
            zod::number().int().positive().optional(),
        ),
        ("deadlineMs", zod::number().int().positive().optional()),
        (
            "maxPerSession",
            zod::number().int().nonnegative().optional(),
        ),
        (
            "maxConcurrentSearches",
            zod::number().int().positive().optional(),
        ),
        ("rerank", web_rerank.optional()),
    ]);
    let web_fetch = zod::object(vec![
        ("timeoutMs", zod::number().int().positive().optional()),
        (
            "connectTimeoutMs",
            zod::number().int().positive().optional(),
        ),
        ("maxRedirects", zod::number().int().nonnegative().optional()),
        (
            "maxPerSession",
            zod::number().int().nonnegative().optional(),
        ),
        (
            "maxBytesPerSession",
            zod::number().int().positive().optional(),
        ),
        (
            "fallbackDeadlineMs",
            zod::number().int().positive().optional(),
        ),
    ]);
    let web = zod::object(vec![
        (
            "providers",
            zod::partial_record(provider_names(), web_provider).optional(),
        ),
        ("search", web_search.optional()),
        ("fetch", web_fetch.optional()),
        (
            "fetchFallback",
            zod::array(zod::enumeration(provider_names())).optional(),
        ),
        (
            "fetchVia",
            zod::enumeration(vec!["proxy", "direct"]).optional(),
        ),
        ("allowHosts", zod::array(zod::string()).optional()),
        ("denyHosts", zod::array(zod::string()).optional()),
        ("allowPorts", zod::array(zod::number().int()).optional()),
        ("maxBytes", zod::number().int().positive().optional()),
        ("maxReturnChars", zod::number().int().positive().optional()),
        (
            "snapshotRetentionDays",
            zod::number().int().nonnegative().optional(),
        ),
        ("extractModel", zod::string().optional()),
    ]);

    let emrank = zod::object(vec![
        ("url", zod::string().optional()),
        ("apiKeyFile", zod::string().optional()),
        ("models", zod::record(zod::string()).optional()),
        (
            "poolIdleTimeoutMs",
            zod::number().int().positive().optional(),
        ),
        ("prewarm", zod::boolean().optional()),
    ]);
    let chat = zod::object(vec![("model", zod::string().optional())]);
    let deadlines = zod::object(vec![
        ("critical", zod::number().int().positive().optional()),
        ("background", zod::number().int().positive().optional()),
    ]);
    let decision_point = zod::object(vec![
        (
            "mode",
            zod::enumeration(vec!["off", "shadow", "active"]).optional(),
        ),
        (
            "model",
            zod::enumeration(vec!["fast", "accurate"]).optional(),
        ),
        ("minConfidence", zod::number().min(0).max(1).optional()),
        ("maxProb", zod::number().min(0).max(1).optional()),
        ("keepAtLeast", zod::number().int().nonnegative().optional()),
    ]);
    let decision = zod::object(vec![
        ("enabled", zod::boolean().optional()),
        ("emrank", emrank.optional()),
        ("chat", chat.optional()),
        ("deadlineMs", deadlines.optional()),
        ("maxBatch", zod::number().int().positive().optional()),
        (
            "maxCallsPerSession",
            zod::number().int().nonnegative().optional(),
        ),
        (
            "maxInputTokensPerSession",
            zod::number().int().nonnegative().optional(),
        ),
        (
            "points",
            zod::partial_record(decision_point_names(), decision_point).optional(),
        ),
        (
            "logRetentionDays",
            zod::number().int().nonnegative().optional(),
        ),
    ]);

    let max_line_chars = zod::object(vec![
        ("read", zod::number().int().positive().optional()),
        ("grep", zod::number().int().positive().optional()),
    ]);
    let tool_io = zod::object(vec![
        ("completionNotes", zod::boolean().optional()),
        ("stableGrepOrder", zod::boolean().optional()),
        ("readPartialOnOverflow", zod::boolean().optional()),
        ("readBatchMax", zod::number().int().nonnegative().optional()),
        ("encodingDetection", zod::boolean().optional()),
        ("contentBinaryCheck", zod::boolean().optional()),
        ("maxLineChars", max_line_chars.optional()),
        ("bashHeadTail", zod::boolean().optional()),
        ("reportUnreachable", zod::boolean().optional()),
    ]);

    zod::object(vec![
        ("providers", zod::record(provider).optional()),
        ("models", zod::record(model).optional()),
        ("selection", selection.optional()),
        ("compaction", compaction.optional()),
        ("cache", cache.optional()),
        ("memory", memory.optional()),
        ("retrieval", retrieval.optional()),
        ("web", web.optional()),
        ("decision", decision.optional()),
        ("toolIo", tool_io.optional()),
    ])
    .describe(
        "nyan settings: provider/model profiles and module settings. Provider profiles are user/managed-only; project/local settings may only select existing profile names.",
    )
}

// ════════════════════════════════════════════════════════════
// Tests
// ════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(value: serde_json::Value) -> NyanSettings {
        serde_json::from_value(value).expect("nyan settings deserialize")
    }

    #[test]
    fn missing_settings_resolve_to_document_defaults() {
        let settings = NyanSettings::default();
        assert!(settings.compaction().mask());
        assert_eq!(settings.compaction().mask_min_fraction(), 0.01);
        assert_eq!(settings.compaction().keep_recent_tokens(), 20_000);
        assert_eq!(settings.compaction().summary_style(), SummaryStyle::Legacy);
        assert_eq!(settings.compaction().mask_headroom(), 0.10);
        assert!(!settings.compaction().keep_shadow());

        assert_eq!(settings.cache().warming(), CacheWarmingMode::Active);
        assert_eq!(settings.cache().min_cached_tokens(), 20_000);
        assert_eq!(settings.cache().max_active_minutes(), 60);
        assert_eq!(settings.cache().max_idle_minutes(), 30);

        assert!(settings.memory().extract());
        assert_eq!(settings.memory().extract_every_turns(), 8);
        assert_eq!(settings.memory().extract_every_tokens(), 30_000);
        assert!(settings.memory().extract_before_compact());
        assert_eq!(
            settings.memory().extract_before_compact_timeout_ms(),
            60_000
        );
        assert_eq!(settings.memory().extract_max_turns(), 6);
        assert_eq!(settings.memory().consolidate(), ConsolidateMode::Auto);
        assert_eq!(settings.memory().consolidate_min_hours(), 24);
        assert_eq!(settings.memory().consolidate_min_new_files(), 5);
        assert!(settings.memory().secret_scan());
        // @nyan: every memory decision-shadow switch defaults to off.
        let shadow = settings.memory().decision_shadow();
        assert!(!shadow.extract_now());
        assert!(!shadow.extract_skip());
        assert!(!shadow.memory_type());
        assert!(!shadow.duplicate());
        assert!(!shadow.consolidate());

        let retrieval = settings.retrieval();
        assert!(retrieval.enabled());
        assert_eq!(retrieval.index().max_file_bytes(), 1_048_576);
        assert_eq!(retrieval.index().languages(), "auto");
        assert!(retrieval.semantic().is_none(), "unconfigured means off");
        assert!(!retrieval.rerank().enabled(), "code rerank is opt-in");
        assert_eq!(retrieval.rerank().url(), "http://10.144.0.11:8091");
        assert_eq!(retrieval.rerank().model(), "qwen3-reranker-0.6b");
        assert_eq!(retrieval.rerank().top_n(), 10);
        assert_eq!(retrieval.rerank().max_document_chars(), 3_000);
        assert_eq!(retrieval.budget().deadline_ms(), 8_000);
        assert_eq!(retrieval.budget().semantic_deadline_ms(), 4_500);
        assert_eq!(retrieval.budget().rrf_k(), 60);
        assert_eq!(retrieval.budget().connect_timeout_ms(), 1_500);
        assert_eq!(retrieval.budget().embed_timeout_ms(), 2_500);
        assert_eq!(retrieval.budget().qdrant_timeout_ms(), 1_500);
        assert_eq!(retrieval.budget().rerank_timeout_ms(), 2_000);
        assert_eq!(retrieval.sync().batch_size(), 128);
        assert_eq!(retrieval.sync().concurrency(), 8);
        assert_eq!(retrieval.sync().embed_batch_timeout_ms(), 60_000);
        assert_eq!(retrieval.sync().qdrant_batch_timeout_ms(), 15_000);
        assert_eq!(retrieval.sync().max_batch_failures(), 3);
        assert_eq!(retrieval.sync().prune_after_days(), 90);

        let web = settings.web();
        let searxng = web.provider("searxng");
        assert!(searxng.enabled);
        assert_eq!(searxng.url.as_deref(), Some("http://10.144.0.3:8454"));
        let exa = web.provider("exa");
        assert!(exa.enabled);
        assert_eq!(exa.api_key_file.as_deref(), Some("~/.nyan/keys/exa"));
        assert_eq!(exa.max_calls_per_session, Some(30));
        assert_eq!(web.search().recall_depth(), 20);
        assert_eq!(web.search().fusion_keep(), 30);
        assert_eq!(web.search().domain_cap(), 3);
        assert_eq!(web.search().rrf_k(), 60);
        assert_eq!(web.search().per_provider_deadline_ms(), 8_000);
        assert_eq!(web.search().deadline_ms(), 10_000);
        assert_eq!(web.search().max_per_session(), 40);
        assert_eq!(web.search().max_concurrent_searches(), 2);
        assert!(
            web.search().rerank().enabled(),
            "web rerank is on by default"
        );
        assert_eq!(web.search().rerank().timeout_ms(), 2_000);
        assert_eq!(web.search().rerank().snippet_chars(), 500);
        assert_eq!(web.fetch().timeout_ms(), 10_000);
        assert_eq!(web.fetch().connect_timeout_ms(), 5_000);
        assert_eq!(web.fetch().max_redirects(), 5);
        assert_eq!(web.fetch().max_per_session(), 60);
        assert_eq!(web.fetch().max_bytes_per_session(), 104_857_600);
        assert_eq!(web.fetch().fallback_deadline_ms(), 30_000);
        assert_eq!(
            web.fallback_order(),
            vec!["firecrawl", "anysearch", "exa", "deepseek", "tavily"]
        );
        assert_eq!(web.fetch_via(), FetchVia::Proxy);
        assert_eq!(web.max_bytes(), 5_242_880);
        assert_eq!(web.max_return_chars(), 20_000);
        assert_eq!(web.snapshot_retention_days(), 30);

        let decision = settings.decision();
        assert!(decision.enabled());
        assert_eq!(decision.emrank().url(), "https://emrank.tech/v1/decisions");
        assert_eq!(decision.emrank().api_key_file(), "~/.nyan/keys/emrank");
        assert_eq!(
            decision.emrank().model_id(DecisionModel::Fast),
            "typesafe/jev-1.14.0"
        );
        assert_eq!(
            decision.emrank().model_id(DecisionModel::Accurate),
            "cloudflare/clef"
        );
        assert_eq!(decision.emrank().pool_idle_timeout_ms(), 90_000);
        assert!(decision.emrank().prewarm());
        assert_eq!(decision.max_batch(), 30);
        assert_eq!(decision.max_calls_per_session(), 600);
        assert_eq!(decision.max_input_tokens_per_session(), 3_000_000);
        assert_eq!(decision.log_retention_days(), 30);
        assert_eq!(decision.deadline(true).as_millis(), 800);
        assert_eq!(decision.deadline(false).as_millis(), 2_000);

        let tool_io = settings.tool_io();
        assert!(tool_io.completion_notes());
        assert!(tool_io.stable_grep_order());
        assert!(tool_io.read_partial_on_overflow());
        assert_eq!(tool_io.read_batch_max(), 16);
        assert!(tool_io.encoding_detection());
        assert!(tool_io.content_binary_check());
        assert_eq!(tool_io.max_line_chars().read(), 2_000);
        assert_eq!(tool_io.max_line_chars().grep(), 500);
        assert!(tool_io.bash_head_tail());
        assert!(tool_io.report_unreachable());
    }

    #[test]
    fn every_decision_point_has_a_builtin_default_with_the_documented_direction() {
        let decision = DecisionSettings::default();
        for point in BUILTIN_DECISION_POINTS {
            let resolved = decision
                .resolved_point(point.name)
                .unwrap_or_else(|| panic!("{} must resolve", point.name));
            assert_eq!(resolved.mode, point.mode, "{} mode", point.name);
            assert_eq!(resolved.model, point.model, "{} model", point.name);
        }
        // Spot checks from decision-provider 4.1–4.4 / 8.
        assert_eq!(
            decision.resolved_point("retrieval.answers").unwrap().mode,
            DecisionMode::Active
        );
        assert_eq!(
            decision.resolved_point("retrieval.prune").unwrap().mode,
            DecisionMode::Shadow
        );
        assert_eq!(
            decision.resolved_point("retrieval.prune").unwrap().max_prob,
            Some(0.05)
        );
        assert_eq!(
            decision
                .resolved_point("retrieval.prune")
                .unwrap()
                .keep_at_least,
            Some(3)
        );
        assert_eq!(
            decision.resolved_point("web.prune").unwrap().keep_at_least,
            Some(5)
        );
        assert_eq!(
            decision.resolved_point("web.fetch_fallback").unwrap().model,
            DecisionModel::Accurate
        );
        assert_eq!(
            decision.resolved_point("agent.route").unwrap().mode,
            DecisionMode::Off
        );
        assert!(DecisionSettings::is_critical_point("retrieval.answers"));
        assert!(DecisionSettings::is_critical_point("web.answers"));
        assert!(DecisionSettings::is_critical_point("compaction.keep"));
        assert!(!DecisionSettings::is_critical_point("memory.type"));
    }

    #[test]
    fn user_overrides_win_per_field_over_builtin_defaults() {
        let settings = parse(json!({
            "decision": {
                "enabled": false,
                "points": {
                    "retrieval.answers": {"mode": "shadow", "minConfidence": 0.4},
                    "retrieval.prune": {"maxProb": 0.02}
                }
            }
        }));
        let decision = settings.decision();
        assert!(!decision.enabled());
        let answers = decision.resolved_point("retrieval.answers").unwrap();
        assert_eq!(answers.mode, DecisionMode::Shadow);
        assert_eq!(answers.min_confidence, 0.4);
        assert_eq!(
            answers.model,
            DecisionModel::Fast,
            "unset field keeps default"
        );
        let prune = decision.resolved_point("retrieval.prune").unwrap();
        assert_eq!(prune.max_prob, Some(0.02));
        assert_eq!(prune.keep_at_least, Some(3), "unset field keeps default");
    }

    #[test]
    fn unset_fields_stay_unset_so_settings_sources_merge_partially() {
        // A source that only turns extraction off must not serialize the rest
        // of the defaults, or it would clobber values from another source.
        let settings: NyanSettings = parse(json!({
            "memory": {"extract": false}
        }));
        let value = serde_json::to_value(&settings).unwrap();
        assert_eq!(value, json!({"memory": {"extract": false}}));
    }

    #[test]
    fn semantic_presence_is_the_enable_switch_and_defaults_fill_in() {
        let absent = NyanSettings::default();
        assert!(absent.retrieval().semantic().is_none());

        let configured = parse(json!({"retrieval": {"semantic": {}}}));
        let semantic = configured
            .retrieval()
            .semantic()
            .expect("empty section still opts in");
        assert_eq!(semantic.embedding().url(), "http://10.144.0.11:8090");
        assert_eq!(semantic.embedding().model(), "jina-code-embeddings-1.5b");
        assert_eq!(semantic.embedding().dim(), 1536);
        assert_eq!(semantic.qdrant().url(), "http://10.144.0.3:6333");
        assert_eq!(
            semantic.qdrant().api_key_file(),
            Some("~/.nyan/keys/qdrant")
        );
    }

    #[test]
    fn rerank_switches_are_independent() {
        let settings = parse(json!({
            "retrieval": {"rerank": {"enabled": true}},
            "web": {"search": {"rerank": {"enabled": false}}}
        }));
        assert!(settings.retrieval_rerank_enabled());
        assert!(!settings.web_rerank_enabled());

        let defaults = NyanSettings::default();
        assert!(!defaults.retrieval_rerank_enabled());
        assert!(defaults.web_rerank_enabled());

        // Turning retrieval off does not turn web rerank off.
        let disabled_retrieval = parse(json!({"retrieval": {"enabled": false}}));
        assert!(disabled_retrieval.web_rerank_enabled());
    }

    #[test]
    fn rerank_top_n_and_document_cap_are_overridable() {
        let settings = parse(json!({
            "retrieval": {"rerank": {"topN": 20, "maxDocumentChars": 1500}}
        }));
        assert!(
            !settings.retrieval().rerank().enabled(),
            "the switch stays opt-in"
        );
        assert_eq!(settings.retrieval().rerank().top_n(), 20);
        assert_eq!(settings.retrieval().rerank().max_document_chars(), 1500);
    }

    #[test]
    fn provider_override_keeps_builtin_key_file_and_only_changes_given_fields() {
        let settings = parse(json!({
            "web": {"providers": {"brave": {"enabled": false}}}
        }));
        let brave = settings.web().provider("brave");
        assert!(!brave.enabled);
        assert_eq!(brave.api_key_file.as_deref(), Some("~/.nyan/keys/brave"));
        assert_eq!(brave.max_calls_per_session, Some(40));
    }

    #[test]
    fn provider_profiles_parse_with_capabilities_and_compatibility_credential_shapes() {
        let value = json!({
            "providers": {
                "anthropic": {
                    "kind": "anthropic",
                    "protocol": "anthropicMessages",
                    "credential": {"env": "ANTHROPIC_API_KEY"}
                },
                "local": {
                    "kind": "vllm",
                    "protocol": "openaiChat",
                    "credential": {"optionalEnv": "VLLM_API_KEY"}
                }
            },
            "models": {
                "main": {
                    "provider": "anthropic",
                    "model": "claude-sonnet-4-6",
                    "capabilities": {
                        "contextWindow": 200000,
                        "maxOutputTokens": 128000,
                        "tools": "native"
                    }
                }
            },
            "selection": {
                "main": "main",
                "mainFallbacks": ["local"],
                "subagent": "inherit",
                "decision": {"fast": "main"}
            }
        });
        let parsed =
            crate::utils::zod::safe_parse(&nyan_settings_schema(), &value).expect("valid profiles");
        let settings: NyanSettings = serde_json::from_value(parsed).unwrap();
        assert_eq!(
            settings
                .models
                .as_ref()
                .unwrap()
                .get("main")
                .unwrap()
                .capabilities
                .as_ref()
                .unwrap()
                .context_window,
            Some(200000)
        );
        assert_eq!(
            settings
                .selection
                .as_ref()
                .unwrap()
                .main_fallbacks
                .as_deref(),
            Some(["local".to_string()].as_slice())
        );
    }

    #[test]
    fn provider_schema_rejects_unknown_kind_commercial_and_credential_shapes() {
        for bad in [
            json!({"providers": {"x": {"kind": "custom", "protocol": "openaiChat"}}}),
            json!({"providers": {"x": {"kind": "openai", "commercial": true}}}),
            json!({"providers": {"x": {"kind": "openai", "credential": {"env": "A", "file": "B"}}}}),
            json!({"models": {"x": {"provider": "p", "model": "m", "capabilities": {"tools": "guess"}}}}),
            json!({"models": {"x": {"provider": "p", "model": "m", "input": ["application/pdf"]}}}),
            json!({"models": {"x": {"provider": "p", "model": "m", "cost": {"input": -1}}}}),
            json!({"models": {"x": {"provider": "p", "model": "m", "cost": {"currency": "USD"}}}}),
        ] {
            assert!(
                crate::utils::zod::safe_parse(&nyan_settings_schema(), &bad).is_err(),
                "should reject {bad}"
            );
        }
    }

    #[test]
    fn schema_accepts_the_documented_example_and_strips_unknown_nested_keys() {
        let value = json!({
            "compaction": {"mask": false, "summaryStyle": "cc"},
            "retrieval": {
                "semantic": {"embedding": {"url": "http://10.144.0.11:8090"}},
                "rerank": {"enabled": true, "topN": 10}
            },
            "web": {"providers": {"searxng": {"enabled": true}}},
            "decision": {"points": {"retrieval.answers": {"mode": "shadow"}}},
            "toolIo": {"readBatchMax": 0},
            "futureKey": {"kept": false}
        });
        let parsed =
            crate::utils::zod::safe_parse(&nyan_settings_schema(), &value).expect("valid nyan");
        assert!(
            parsed.get("futureKey").is_none(),
            "unknown keys are stripped"
        );
        let settings: NyanSettings = serde_json::from_value(parsed).unwrap();
        assert!(!settings.compaction().mask());
        assert_eq!(settings.compaction().summary_style(), SummaryStyle::Cc);
        assert!(settings.retrieval().semantic().is_some());
        assert!(settings.retrieval().rerank().enabled());
        assert!(settings.web().provider("searxng").enabled);
        assert_eq!(
            settings
                .decision()
                .resolved_point("retrieval.answers")
                .unwrap()
                .mode,
            DecisionMode::Shadow
        );
        assert_eq!(settings.tool_io().read_batch_max(), 0);
    }

    #[test]
    fn schema_rejects_invalid_enums_numbers_and_unknown_point_names() {
        for bad in [
            json!({"compaction": {"summaryStyle": "fancy"}}),
            json!({"cache": {"warming": "sometimes"}}),
            json!({"memory": {"consolidate": "whenever"}}),
            json!({"web": {"fetchVia": "carrier-pigeon"}}),
            json!({"decision": {"points": {"retrieval.unknown": {"mode": "active"}}}}),
            json!({"decision": {"points": {"retrieval.answers": {"mode": "loud"}}}}),
            json!({"web": {"providers": {"myspace": {"enabled": true}}}}),
            json!({"compaction": {"maskMinFraction": 2.0}}),
            json!({"retrieval": {"budget": {"deadlineMs": -1}}}),
            json!({"decision": {"emrank": {"prewarm": "yes"}}}),
        ] {
            assert!(
                crate::utils::zod::safe_parse(&nyan_settings_schema(), &bad).is_err(),
                "should reject {bad}"
            );
        }
    }

    #[test]
    fn max_return_chars_is_capped_at_forty_thousand() {
        let settings = parse(json!({"web": {"maxReturnChars": 100_000}}));
        assert_eq!(settings.web().max_return_chars(), 40_000);
    }
}
