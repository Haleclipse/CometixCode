//! @nyan: optional embedding/Qdrant branch (docs/nyan/retrieval.md 4.8).
//! It is deliberately isolated from the deterministic local branches: an
//! unavailable corp-pc or Qdrant never turns a code search into an empty
//! success.
//!
//! Production properties implemented here:
//! - base64 `encoding_format` for every embedding request (one wire format for
//!   loopback and tailnet alike);
//! - readiness is more than HTTP 200: `/v1/models` names the configured model,
//!   the vector has the configured dimension, finite values, and L2 norm ≈ 1;
//! - one collection per `(worktreeId, semanticGeneration)`; payload carries
//!   path/lines/chunkId/fileHashRaw/symbol — never source text;
//! - a single pending queue (`semantic-pending.jsonl`) is the sync state;
//!   batches keep their entries on failure and are retried in the background;
//! - replaced generations are pruned from Qdrant after a successful switch;
//! - service health is the shared `Ready ⇄ Down` machine, so a powered-off
//!   corp-pc costs at most one connect timeout, then the branch is skipped.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::services::service_health::{self, CallPermit, FailureKind, ServiceKind};
use crate::utils::settings::nyan::{RetrievalSyncSettings, SemanticSettings};

use super::index::{CHUNKER_VERSION, IndexLock, LocalIndex};
use super::outcome::{Branch, BranchStatus, Candidate, SearchRequest};

pub const QUERY_PREFIX: &str = "Find the most relevant code snippet given the following query:\n";
const GENERATION_VERSION: &str = "semantic-v2";
const SEARCH_LIMIT: usize = 50;
const MODELS_PROBE_LOG: &str = "semantic readiness";

#[derive(Clone)]
pub struct SemanticClient {
    embedding: EmbeddingClient,
    qdrant: QdrantClient,
    pub generation: String,
    pub collection: String,
    pub worktree_id: String,
}

#[derive(Clone)]
struct EmbeddingClient {
    url: String,
    model: String,
    dim: usize,
    api_key: Option<String>,
    client: reqwest::Client,
    health: Arc<service_health::ServiceHealth>,
    /// Query-side budget. Sync batches pass their own (larger) deadline to
    /// [`Self::embed_batch`] instead of inheriting this one.
    request_timeout: Duration,
}

#[derive(Clone)]
struct QdrantClient {
    url: String,
    api_key: Option<String>,
    client: reqwest::Client,
    health: Arc<service_health::ServiceHealth>,
    /// Query-side request budget (`budget.qdrantTimeoutMs`). Sync batches pass
    /// their own, larger `sync.qdrantBatchTimeoutMs` per call.
    query_timeout: Duration,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SemanticSearchResult {
    pub candidates: Vec<Candidate>,
    pub status: BranchStatus,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SyncReport {
    pub synced_chunks: usize,
    pub pending_files: usize,
    pub status: BranchStatus,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PendingEntry {
    path: String,
    #[serde(rename = "fileHashRaw")]
    file_hash_raw: String,
}

/// Local bookkeeping for collection pruning. Only names this machine created
/// are ever deleted; another machine's collections are invisible here.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct CollectionLedger {
    #[serde(default)]
    collections: Vec<LedgerEntry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LedgerEntry {
    collection: String,
    generation: String,
    #[serde(rename = "createdAtMs")]
    created_at_ms: u128,
    #[serde(rename = "lastSyncMs")]
    last_sync_ms: u128,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PruneReport {
    pub deleted: Vec<String>,
    pub kept: Vec<String>,
    pub dry_run: bool,
}

impl SemanticClient {
    pub fn from_settings(
        settings: &SemanticSettings,
        root: &Path,
        budget: &crate::utils::settings::nyan::RetrievalBudgetSettings,
    ) -> anyhow::Result<Self> {
        crate::utils::tls_provider::install_crypto_provider();
        let embedding_settings = settings.embedding();
        let qdrant_settings = settings.qdrant();
        let connect_timeout = Duration::from_millis(budget.connect_timeout_ms());
        let embed_timeout = Duration::from_millis(budget.embed_timeout_ms());
        let qdrant_timeout = Duration::from_millis(budget.qdrant_timeout_ms());
        let embedding_health = service_health::shared(ServiceKind::Embedding);
        let qdrant_health = service_health::shared(ServiceKind::Qdrant);
        let embedding_client = crate::utils::proxy::client_builder()
            .no_proxy()
            .connect_timeout(connect_timeout)
            // No client-wide request timeout: the query path uses the short
            // budget while sync batches use `embedBatchTimeoutMs`, and a
            // shared cap would silently truncate the larger one.
            //
            // vLLM sits behind uvicorn, whose keep-alive is 5 s; the pool
            // window stays under it, and a stale socket is retried once.
            .pool_idle_timeout(Duration::from_secs(4))
            .build()?;
        let qdrant_client = crate::utils::proxy::client_builder()
            .no_proxy()
            .connect_timeout(connect_timeout)
            // No client-wide timeout: the query path caps at
            // `budget.qdrantTimeoutMs` (1.5 s) while a sync batch gets
            // `sync.qdrantBatchTimeoutMs` (15 s); a shared cap would truncate
            // the larger one (a 128-point upsert measured 0.8–2.1 s).
            //
            // nyan-ops sits behind a ~260 ms path; idle pooled sockets go
            // stale between searches, so keep the window short and retry.
            .pool_idle_timeout(Duration::from_secs(4))
            .build()?;
        let embedding = EmbeddingClient {
            url: embedding_settings.url().trim_end_matches('/').to_string(),
            model: embedding_settings.model().to_string(),
            dim: embedding_settings.dim() as usize,
            api_key: read_key(embedding_settings.api_key_file.as_deref())?,
            client: embedding_client,
            health: embedding_health,
            request_timeout: embed_timeout,
        };
        let qdrant = QdrantClient {
            url: qdrant_settings.url().trim_end_matches('/').to_string(),
            api_key: read_key(qdrant_settings.api_key_file())?,
            client: qdrant_client,
            health: qdrant_health,
            query_timeout: qdrant_timeout,
        };
        embedding.install_probe();
        qdrant.install_probe();
        let worktree_id = worktree_id(root);
        let generation = generation(&embedding, &worktree_id);
        let collection = format!(
            "nyan_{worktree_id}_{}",
            &generation[7..23.min(generation.len())]
        );
        Ok(Self {
            embedding,
            qdrant,
            generation,
            collection,
            worktree_id,
        })
    }

    pub fn generation(&self) -> &str {
        &self.generation
    }

    pub fn collection(&self) -> &str {
        &self.collection
    }

    pub async fn search(
        &self,
        index: &LocalIndex,
        request: &SearchRequest,
        deadline: Duration,
    ) -> SemanticSearchResult {
        if deadline.is_zero() {
            return SemanticSearchResult {
                candidates: Vec::new(),
                status: BranchStatus::TimedOut,
            };
        }
        match tokio::time::timeout(deadline, self.search_inner(index, request)).await {
            Ok(result) => result,
            Err(_) => SemanticSearchResult {
                candidates: Vec::new(),
                status: BranchStatus::TimedOut,
            },
        }
    }

    async fn search_inner(
        &self,
        index: &LocalIndex,
        request: &SearchRequest,
    ) -> SemanticSearchResult {
        if self.embedding.health.is_down() || self.qdrant.health.is_down() {
            return SemanticSearchResult {
                candidates: Vec::new(),
                status: BranchStatus::Unavailable {
                    reason: "corp-pc unreachable".to_string(),
                },
            };
        }
        if let Err(reason) = self.embedding.readiness().await {
            return SemanticSearchResult {
                candidates: Vec::new(),
                status: BranchStatus::Unavailable { reason },
            };
        }
        let query = format!("{QUERY_PREFIX}{}", request.query);
        let vector = match self.embedding.embed(&[query]).await {
            Ok(mut vectors) => vectors.pop().unwrap_or_default(),
            Err(error) => {
                return SemanticSearchResult {
                    candidates: Vec::new(),
                    status: BranchStatus::Unavailable { reason: error },
                };
            }
        };
        let points = match self
            .qdrant
            .search(&self.collection, &vector, SEARCH_LIMIT)
            .await
        {
            Ok(points) => points,
            Err(error) => {
                return SemanticSearchResult {
                    candidates: Vec::new(),
                    status: BranchStatus::Unavailable { reason: error },
                };
            }
        };
        let mut candidates = Vec::new();
        for (rank, point) in points.into_iter().enumerate() {
            let payload = point.get("payload").unwrap_or(&point);
            let Some(path) = payload.get("path").and_then(Value::as_str) else {
                continue;
            };
            let Some(file_hash_raw) = payload.get("fileHashRaw").and_then(Value::as_str) else {
                continue;
            };
            let start_line = payload
                .get("startLine")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            let end_line = payload
                .get("endLine")
                .and_then(Value::as_u64)
                .unwrap_or(start_line as u64) as usize;
            if start_line == 0 {
                continue;
            }
            let symbol = payload
                .get("symbol")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let normalized_path = path.replace('\\', "/");
            if !super::index::matches_request(&normalized_path, request) {
                continue;
            }
            let no_symbol_check = index
                .file(&normalized_path)
                .is_none_or(|file| !file.symbol_checked);
            candidates.push(Candidate {
                branch: Branch::Semantic,
                path: normalized_path,
                start_line,
                end_line,
                symbol,
                file_hash_raw: file_hash_raw.to_string(),
                rank: rank + 1,
                score: point.get("score").and_then(Value::as_f64),
                no_symbol_check,
                ..Candidate::default()
            });
        }
        SemanticSearchResult {
            candidates,
            status: BranchStatus::Ok,
        }
    }

    /// Replace the queue with one entry per changed file of the current index
    /// generation. Deduplicated by `(path, fileHashRaw)`; deleted files leave
    /// the pending queue and enter the delete queue, because Qdrant cleanup is
    /// async and must not be attempted from this synchronous path.
    pub fn queue_changed(&self, index: &LocalIndex) -> Result<usize, String> {
        let queue_path = index.storage().join("semantic-pending.jsonl");
        let delete_path = index.storage().join("semantic-deleted.jsonl");
        let mut entries = read_queue(&queue_path);
        let mut deleted = read_queue(&delete_path);
        let mut known = entries
            .iter()
            .map(|entry| (entry.path.clone(), entry.file_hash_raw.clone()))
            .collect::<HashSet<_>>();
        let mut changed = false;
        for path in index.removed_files() {
            entries.retain(|entry| entry.path != *path);
            if !deleted.iter().any(|entry| entry.path == *path) {
                deleted.push(PendingEntry {
                    path: path.clone(),
                    file_hash_raw: String::new(),
                });
            }
            changed = true;
        }
        for path in index.changed_files() {
            let Some(file) = index.file(path) else {
                continue;
            };
            // A changed file supersedes every older hash of itself, and it can
            // no longer be pending deletion.
            deleted.retain(|entry| entry.path != *path);
            let before = entries.len();
            entries
                .retain(|entry| entry.path != *path || entry.file_hash_raw == file.file_hash_raw);
            if entries.len() == before
                && !known.insert((file.path.clone(), file.file_hash_raw.clone()))
            {
                continue;
            }
            entries.push(PendingEntry {
                path: file.path.clone(),
                file_hash_raw: file.file_hash_raw.clone(),
            });
            changed = true;
        }
        if changed {
            write_queue(&queue_path, &entries)?;
            write_queue(&delete_path, &deleted)?;
        }
        Ok(entries.len())
    }

    pub fn pending_count(index: &LocalIndex) -> usize {
        read_queue(&index.storage().join("semantic-pending.jsonl")).len()
    }

    /// Queue the first full synchronization exactly once per index generation.
    pub fn ensure_initial_pending(&self, index: &LocalIndex) -> Result<usize, String> {
        let marker = index
            .storage()
            .join(format!("semantic-initialized-{}", self.generation));
        if marker.is_file() {
            return Ok(Self::pending_count(index));
        }
        let queue_path = index.storage().join("semantic-pending.jsonl");
        let mut entries = read_queue(&queue_path);
        let mut known = entries
            .iter()
            .map(|entry| (entry.path.clone(), entry.file_hash_raw.clone()))
            .collect::<HashSet<_>>();
        for file in index.files() {
            if known.insert((file.path.clone(), file.file_hash_raw.clone())) {
                entries.push(PendingEntry {
                    path: file.path.clone(),
                    file_hash_raw: file.file_hash_raw.clone(),
                });
            }
        }
        entries.sort_by(|left, right| {
            left.path
                .as_bytes()
                .cmp(right.path.as_bytes())
                .then_with(|| left.file_hash_raw.cmp(&right.file_hash_raw))
        });
        write_queue(&queue_path, &entries)?;
        std::fs::write(&marker, b"v1").map_err(|error| error.to_string())?;
        Ok(entries.len())
    }

    /// Synchronize pending files in text order.
    ///
    /// Chunks from every file in a window are pooled into batches of
    /// `sync.batchSize` and the batches are embedded and upserted with
    /// `sync.concurrency` in flight. Batching per file instead would pay one
    /// Qdrant round trip (~0.7–0.9 s from nyan-pc) for a median of 11 chunks
    /// and make the first full sync take minutes longer than the bytes
    /// justify.
    ///
    /// A file leaves the queue only after every chunk it contributed has been
    /// embedded and upserted; a failed batch keeps its files queued and counts
    /// toward the service health circuit breaker, which stops the run instead
    /// of spinning through the rest of a dead service.
    pub async fn sync_pending(
        &self,
        index: &LocalIndex,
        settings: &RetrievalSyncSettings,
    ) -> SyncReport {
        let pending_before = Self::pending_count(index);
        if pending_before == 0 {
            return SyncReport {
                synced_chunks: 0,
                pending_files: 0,
                status: BranchStatus::Ok,
            };
        }
        if self.embedding.health.is_down() || self.qdrant.health.is_down() {
            return SyncReport {
                synced_chunks: 0,
                pending_files: pending_before,
                status: BranchStatus::Partial {
                    pending: pending_before,
                },
            };
        }
        let Some(_lock) = IndexLock::acquire(index.storage()) else {
            return SyncReport {
                synced_chunks: 0,
                pending_files: pending_before,
                status: BranchStatus::Partial {
                    pending: pending_before,
                },
            };
        };
        let queue_path = index.storage().join("semantic-pending.jsonl");
        let delete_path = index.storage().join("semantic-deleted.jsonl");
        let entries = read_queue(&queue_path);
        let batch_size = settings.batch_size().max(1) as usize;
        let embed_timeout = Duration::from_millis(settings.embed_batch_timeout_ms().max(1_000));
        let qdrant_timeout = Duration::from_millis(settings.qdrant_batch_timeout_ms().max(1_000));
        let max_failures = settings.max_batch_failures().max(1);
        // Batches in flight, not files: one batch is one embedding request
        // plus one Qdrant upsert.
        let concurrency = settings.concurrency().max(1) as usize;
        // Files prepared per window: enough to fill several batches without
        // holding the whole worktree's text in memory.
        let window_files = (concurrency * 8).max(8);
        let mut remaining = Vec::new();
        let mut synced_chunks = 0;
        let mut consecutive_failures = 0u32;
        let mut failure: Option<String> = None;
        let started = std::time::Instant::now();
        if let Err(reason) = self
            .qdrant
            .ensure_collection(&self.collection, self.embedding.dim, qdrant_timeout)
            .await
        {
            return SyncReport {
                synced_chunks: 0,
                pending_files: pending_before,
                status: BranchStatus::Unavailable { reason },
            };
        }

        // Deleted-file cleanup first: those payloads must not survive a sync.
        let deleted = read_queue(&delete_path);
        if !deleted.is_empty() {
            let mut still_deleted = Vec::new();
            for entry in deleted {
                match self
                    .qdrant
                    .delete_path(&self.collection, &entry.path, qdrant_timeout)
                    .await
                {
                    Ok(()) => {}
                    Err(error) => {
                        failure = Some(error);
                        still_deleted.push(entry);
                    }
                }
            }
            if failure.is_some() {
                write_queue(&delete_path, &still_deleted).ok();
                write_queue(&queue_path, &entries).ok();
                return SyncReport {
                    synced_chunks: 0,
                    pending_files: entries.len(),
                    status: BranchStatus::Unavailable {
                        reason: failure.unwrap_or_default(),
                    },
                };
            }
            write_queue(&delete_path, &still_deleted).ok();
        }

        let mut pending_entries = entries;
        'windows: loop {
            if pending_entries.is_empty() {
                break;
            }
            let window_size = pending_entries.len().min(window_files);
            let window: Vec<PendingEntry> = pending_entries.drain(..window_size).collect();
            let mut files: Vec<FileWork> = Vec::with_capacity(window.len());
            for entry in window {
                let prepared = match index.file_chunks(&entry.path) {
                    Some((file, texts)) if file.file_hash_raw == entry.file_hash_raw => {
                        Some((file, texts))
                    }
                    // Changed since indexing: a newer queue entry owns the file;
                    // a vanished file has a delete-queue entry. Either way this
                    // entry is done.
                    Some(_) => None,
                    None => {
                        if index
                            .file(&entry.path)
                            .is_some_and(|file| file.file_hash_raw != entry.file_hash_raw)
                        {
                            None
                        } else if index.file(&entry.path).is_none() {
                            None
                        } else {
                            // Unreadable right now (transient I/O): retry later.
                            remaining.push(entry);
                            continue;
                        }
                    }
                };
                if let Some((file, texts)) = prepared {
                    files.push(FileWork {
                        entry,
                        file,
                        remaining_chunks: texts.len(),
                        chunks: texts,
                        failed: false,
                    });
                }
            }

            // Flatten the window into batches of `batch_size` chunks,
            // preserving file order inside each batch. A batch owns its
            // documents and point payloads so its future borrows nothing from
            // this window.
            let mut tasks: Vec<BatchTask> = Vec::new();
            // Batch membership for bookkeeping; the tasks themselves own the
            // documents.
            let mut batch_refs: Vec<Vec<FileChunks>> = Vec::new();
            let mut texts = Vec::with_capacity(batch_size);
            let mut points = Vec::with_capacity(batch_size);
            let mut refs: Vec<FileChunks> = Vec::with_capacity(batch_size);
            for (file_index, work) in files.iter().enumerate() {
                let mut chunks_in_batch = 0usize;
                for (chunk, text) in &work.chunks {
                    let id = chunk_id(&work.file.path, chunk.start_line, chunk.end_line);
                    points.push((
                        id,
                        json!({
                            "path": work.file.path,
                            "startLine": chunk.start_line,
                            "endLine": chunk.end_line,
                            "chunkId": id.to_string(),
                            "fileHashRaw": work.file.file_hash_raw,
                            "symbol": chunk.symbol,
                        }),
                    ));
                    texts.push(text.clone());
                    chunks_in_batch += 1;
                    if texts.len() == batch_size {
                        refs.push(FileChunks {
                            file: file_index,
                            chunks: chunks_in_batch,
                        });
                        tasks.push(BatchTask {
                            texts: std::mem::take(&mut texts),
                            points: std::mem::take(&mut points),
                        });
                        batch_refs.push(std::mem::take(&mut refs));
                        texts.reserve(batch_size);
                        points.reserve(batch_size);
                        refs.reserve(batch_size);
                        chunks_in_batch = 0;
                    }
                }
                if chunks_in_batch > 0 {
                    refs.push(FileChunks {
                        file: file_index,
                        chunks: chunks_in_batch,
                    });
                }
            }
            if !texts.is_empty() {
                tasks.push(BatchTask { texts, points });
                batch_refs.push(refs);
            }

            let results = {
                use futures::StreamExt as _;
                let client = self;
                let collection = self.collection.clone();
                let futures = tasks
                    .into_iter()
                    .enumerate()
                    .map(move |(batch_index, task)| {
                        let documents = task.texts;
                        let points = task.points;
                        let collection = collection.clone();
                        async move {
                            let batch_len = documents.len();
                            let vectors = match client
                                .embedding
                                .embed_batch(&documents, embed_timeout)
                                .await
                            {
                                Ok(vectors) if vectors.len() == batch_len => vectors,
                                Ok(_) => {
                                    client
                                        .embedding
                                        .health
                                        .record_failure(FailureKind::InvalidResponse);
                                    return (
                                        batch_index,
                                        batch_len,
                                        Err("embedding count mismatch".to_string()),
                                    );
                                }
                                Err(error) => return (batch_index, batch_len, Err(error)),
                            };
                            let points = vectors
                            .into_iter()
                            .zip(points)
                            .map(|(vector, (id, payload))| {
                                json!({"id": id, "vector": vector, "payload": payload})
                            })
                            .collect::<Vec<_>>();
                            let result = client
                                .qdrant
                                .upsert(&collection, points, qdrant_timeout)
                                .await;
                            (batch_index, batch_len, result)
                        }
                    });
                futures::stream::iter(futures)
                    .buffer_unordered(concurrency)
                    .collect::<Vec<_>>()
                    .await
            };

            let mut window_failures = 0u32;
            for (batch_index, batch_len, result) in results {
                match result {
                    Ok(()) => {
                        consecutive_failures = 0;
                        for file_chunks in &batch_refs[batch_index] {
                            files[file_chunks.file].remaining_chunks -= file_chunks.chunks;
                        }
                        synced_chunks += batch_len;
                    }
                    Err(error) => {
                        failure.get_or_insert(error);
                        for file_chunks in &batch_refs[batch_index] {
                            files[file_chunks.file].failed = true;
                        }
                        consecutive_failures += 1;
                        window_failures += 1;
                    }
                }
            }
            for work in files {
                if !work.failed && work.remaining_chunks == 0 {
                    continue;
                }
                remaining.push(work.entry);
            }
            // The queue is the progress record: persist failed files plus the
            // untouched tail after every window. An interrupted sync must not
            // lose entries that have not reached a worker yet.
            let mut checkpoint = remaining.clone();
            checkpoint.extend(pending_entries.iter().cloned());
            write_queue(&queue_path, &checkpoint).ok();
            crate::utils::debug::log_for_debugging(&format!(
                "retrieval semantic sync: synced_chunks={} pending_files={} elapsed_ms={}",
                synced_chunks,
                remaining.len(),
                started.elapsed().as_millis()
            ));
            if window_failures > 0 && consecutive_failures >= max_failures {
                // The circuit breaker stops this run, but the tail was never
                // attempted: keep it queued instead of dropping it.
                remaining.extend(pending_entries);
                break 'windows;
            }
        }
        write_queue(&queue_path, &remaining).ok();
        let pending = remaining.len();
        let status = match failure {
            Some(reason) => BranchStatus::Unavailable { reason },
            None if pending == 0 => BranchStatus::Ok,
            None => BranchStatus::Partial { pending },
        };
        if pending == 0 && status == BranchStatus::Ok {
            let _ = self.record_collection(index);
            let _ = self.prune_replaced_collections(index).await;
        }
        SyncReport {
            synced_chunks,
            pending_files: pending,
            status,
        }
    }

    /// Queue all pending files for the background worker. Idempotent; the
    /// worker calls [`Self::sync_pending`] under the single-writer lock.
    pub fn spawn_background_sync(
        &self,
        index: LocalIndex,
        settings: RetrievalSyncSettings,
    ) -> tokio::task::JoinHandle<SyncReport> {
        let client = self.clone();
        tokio::spawn(async move { client.sync_pending(&index, &settings).await })
    }

    /// Delete collections this machine created for an older generation once
    /// the new one has synced. Only names recorded in this index's ledger and
    /// matching this machine's prefix are ever touched.
    pub async fn prune_replaced_collections(
        &self,
        index: &LocalIndex,
    ) -> Result<Vec<String>, String> {
        let mut ledger = read_ledger(index.storage());
        let mut deleted = Vec::new();
        let mut kept = Vec::new();
        for entry in ledger.collections.iter() {
            if entry.collection == self.collection {
                kept.push(entry.clone());
            } else if self
                .qdrant
                .delete_collection(&entry.collection, self.qdrant.query_timeout)
                .await
                .is_ok()
            {
                deleted.push(entry.collection.clone());
            } else {
                kept.push(entry.clone());
            }
        }
        ledger.collections = kept;
        write_ledger(index.storage(), &ledger)?;
        Ok(deleted)
    }

    /// `/retrieval prune [--dry-run]`: list this machine's collections that are
    /// either replaced (ledger entries not current) or not synced for
    /// `prune_after_days`, then delete them unless `dry_run`.
    pub async fn prune(
        &self,
        index: &LocalIndex,
        settings: &RetrievalSyncSettings,
        dry_run: bool,
    ) -> Result<PruneReport, String> {
        let mut ledger = read_ledger(index.storage());
        let now = unix_now_ms();
        let max_age_ms = u128::from(settings.prune_after_days()) * 86_400_000;
        let qdrant_timeout = Duration::from_millis(settings.qdrant_batch_timeout_ms().max(1_000));
        let current = ledger
            .collections
            .iter()
            .find(|entry| entry.collection == self.collection)
            .cloned();
        let mut deleted = Vec::new();
        let mut kept = Vec::new();
        let mut next = Vec::new();
        for entry in ledger.collections.iter().cloned() {
            let stale = entry.collection != self.collection
                && now.saturating_sub(entry.last_sync_ms) > max_age_ms;
            let replaced = entry.collection != self.collection && current.is_some();
            if stale || replaced {
                if dry_run {
                    deleted.push(entry.collection.clone());
                    next.push(entry);
                } else if self
                    .qdrant
                    .delete_collection(&entry.collection, qdrant_timeout)
                    .await
                    .is_ok()
                {
                    deleted.push(entry.collection.clone());
                } else {
                    kept.push(entry.collection.clone());
                    next.push(entry);
                }
            } else {
                kept.push(entry.collection.clone());
                next.push(entry);
            }
        }
        if !dry_run {
            ledger.collections = next;
            write_ledger(index.storage(), &ledger)?;
        }
        Ok(PruneReport {
            deleted,
            kept,
            dry_run,
        })
    }

    fn record_collection(&self, index: &LocalIndex) -> Result<(), String> {
        let mut ledger = read_ledger(index.storage());
        let now = unix_now_ms();
        if let Some(entry) = ledger
            .collections
            .iter_mut()
            .find(|entry| entry.collection == self.collection)
        {
            entry.last_sync_ms = now;
        } else {
            ledger.collections.push(LedgerEntry {
                collection: self.collection.clone(),
                generation: self.generation.clone(),
                created_at_ms: now,
                last_sync_ms: now,
            });
        }
        write_ledger(index.storage(), &ledger)
    }
}

// ────────────────────────────────────────────────────────────
// Queue + ledger helpers
// ────────────────────────────────────────────────────────────

fn read_queue(path: &Path) -> Vec<PendingEntry> {
    std::fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .filter_map(|line| serde_json::from_str::<PendingEntry>(line).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn write_queue(path: &Path, entries: &[PendingEntry]) -> Result<(), String> {
    let temporary = path.with_extension("jsonl.tmp");
    let body = entries
        .iter()
        .filter_map(|entry| serde_json::to_string(entry).ok())
        .collect::<Vec<_>>()
        .join("\n");
    let body = if body.is_empty() {
        String::new()
    } else {
        format!("{body}\n")
    };
    std::fs::write(&temporary, body).map_err(|error| error.to_string())?;
    std::fs::rename(temporary, path).map_err(|error| error.to_string())
}

fn ledger_path(storage: &Path) -> PathBuf {
    storage.join("semantic-collections.json")
}

fn read_ledger(storage: &Path) -> CollectionLedger {
    std::fs::read_to_string(ledger_path(storage))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_ledger(storage: &Path, ledger: &CollectionLedger) -> Result<(), String> {
    let path = ledger_path(storage);
    let temporary = path.with_extension("json.tmp");
    std::fs::write(
        &temporary,
        serde_json::to_vec(ledger).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    std::fs::rename(temporary, path).map_err(|error| error.to_string())
}

fn chunk_id(path: &str, start: usize, end: usize) -> u64 {
    // Qdrant point ids accept u64 or UUID. Derive a stable u64 from the
    // chunk coordinates so re-sync of the same chunk overwrites in place.
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in format!("{path}:{start}:{end}").bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// One queued file prepared for this sync run: the chunk texts read from the
/// worktree plus how many of them still need a successful batch.
struct FileWork {
    entry: PendingEntry,
    file: super::index::IndexedFile,
    chunks: Vec<(super::index::ChunkRecord, String)>,
    remaining_chunks: usize,
    failed: bool,
}

/// How many chunks of one file a batch contains. Batches are built across
/// files so a median file (11 chunks) does not pay its own Qdrant RTT.
struct FileChunks {
    file: usize,
    chunks: usize,
}

/// One embedding→upsert round trip. It owns its documents and point payloads
/// so the concurrent futures do not borrow the window state.
struct BatchTask {
    texts: Vec<String>,
    points: Vec<(u64, Value)>,
}

fn unix_now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

// ────────────────────────────────────────────────────────────
// Embedding client
// ────────────────────────────────────────────────────────────

impl EmbeddingClient {
    fn install_probe(&self) {
        let client = self.client.clone();
        let url = self.url.clone();
        let model = self.model.clone();
        self.health.set_probe(Some(Arc::new(move || {
            let client = client.clone();
            let url = url.clone();
            let model = model.clone();
            Box::pin(async move {
                let response = client
                    .get(format!("{url}/v1/models"))
                    .send()
                    .await
                    .map_err(|_| ())?;
                if !response.status().is_success() {
                    return Err(());
                }
                let body: Value = response.json().await.map_err(|_| ())?;
                let served = body
                    .get("data")
                    .and_then(Value::as_array)
                    .is_some_and(|models| {
                        models.iter().any(|entry| {
                            entry.get("id").and_then(Value::as_str) == Some(model.as_str())
                        })
                    });
                served.then_some(()).ok_or(())
            })
        })));
    }

    /// Legacy 09 readiness: HTTP 200 is not enough. Check the model id, then
    /// embed one probe string and validate dimension, finiteness and norm.
    /// Runs at most once every 10 minutes per process.
    async fn readiness(&self) -> Result<(), String> {
        static LAST_CHECK: std::sync::Mutex<Option<(String, std::time::Instant)>> =
            std::sync::Mutex::new(None);
        let key = format!("{}|{}|{}", self.url, self.model, self.dim);
        if let Ok(guard) = LAST_CHECK.lock()
            && let Some((last_key, at)) = guard.as_ref()
            && last_key == &key
            && at.elapsed() < Duration::from_secs(600)
        {
            return Ok(());
        }
        match self.health.call_permit() {
            CallPermit::Skipped => return Err("embedding service unreachable".to_string()),
            CallPermit::Probe => {
                self.health.spawn_probe();
                return Err("embedding service unreachable".to_string());
            }
            CallPermit::Allowed => {}
        }
        let response = self
            .client
            .get(format!("{}/v1/models", self.url))
            .send()
            .await
            .map_err(|error| {
                self.health.record_failure(if error.is_timeout() {
                    FailureKind::Timeout
                } else {
                    FailureKind::Unreachable
                });
                "embedding service unreachable".to_string()
            })?;
        if !response.status().is_success() {
            self.health.record_failure(FailureKind::Unreachable);
            return Err(format!("embedding /v1/models HTTP {}", response.status()));
        }
        let body: Value = response.json().await.map_err(|_| {
            self.health.record_failure(FailureKind::InvalidResponse);
            "embedding /v1/models is not JSON".to_string()
        })?;
        let served = body
            .get("data")
            .and_then(Value::as_array)
            .is_some_and(|models| {
                models.iter().any(|entry| {
                    entry.get("id").and_then(Value::as_str) == Some(self.model.as_str())
                })
            });
        if !served {
            self.health.record_failure(FailureKind::InvalidResponse);
            return Err(format!("embedding model {} is not served", self.model));
        }
        tracing::info!(target: "nyan::retrieval", probe = MODELS_PROBE_LOG, "embedding model ready");
        if let Ok(mut guard) = LAST_CHECK.lock() {
            *guard = Some((key, std::time::Instant::now()));
        }
        Ok(())
    }

    async fn embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, String> {
        self.embed_batch(inputs, self.request_timeout).await
    }

    /// Embed with an explicit deadline. Sync callers pass
    /// `sync.embedBatchTimeoutMs`; the transport-level retry below applies to
    /// both paths.
    async fn embed_batch(
        &self,
        inputs: &[String],
        timeout: Duration,
    ) -> Result<Vec<Vec<f32>>, String> {
        match self.health.call_permit() {
            CallPermit::Skipped => return Err("embedding service unreachable".to_string()),
            CallPermit::Probe => {
                self.health.spawn_probe();
                return Err("embedding service unreachable".to_string());
            }
            CallPermit::Allowed => {}
        }
        // 3.1: one base64 wire format for every machine.
        // Embedding is a pure function of the input, so a transport-level
        // failure (a pooled socket the server closed, a reset) is retried
        // once; HTTP status errors and timeouts are not.
        let mut attempt = 0;
        let response = tokio::time::timeout(timeout, async {
            loop {
                attempt += 1;
                let request = self
                    .client
                    .post(format!("{}/v1/embeddings", self.url))
                    .json(&json!({
                        "model": self.model,
                        "input": inputs,
                        "encoding_format": "base64",
                    }));
                let request = match self.api_key.as_deref() {
                    Some(key) => request.bearer_auth(key),
                    None => request,
                };
                match request.send().await {
                    Ok(response) => break Ok(response),
                    Err(error) if attempt == 1 && !error.is_timeout() => continue,
                    Err(error) => break Err(error),
                }
            }
        })
        .await;
        let response = match response {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                self.health.record_failure(if error.is_timeout() {
                    FailureKind::Timeout
                } else {
                    FailureKind::Unreachable
                });
                return Err(error.to_string());
            }
            Err(_) => {
                self.health.record_failure(FailureKind::Timeout);
                return Err("embedding request timed out".to_string());
            }
        };
        if !response.status().is_success() {
            self.health
                .record_failure(if response.status().as_u16() == 429 {
                    FailureKind::RateLimited
                } else {
                    FailureKind::Unreachable
                });
            return Err(format!("embedding HTTP {}", response.status()));
        }
        let body: Value = response.json().await.map_err(|error| {
            self.health.record_failure(FailureKind::InvalidResponse);
            error.to_string()
        })?;
        let Some(data) = body.get("data").and_then(Value::as_array) else {
            self.health.record_failure(FailureKind::InvalidResponse);
            return Err("embedding response has no data".to_string());
        };
        if data.len() != inputs.len() {
            self.health.record_failure(FailureKind::InvalidResponse);
            return Err("embedding count mismatch".to_string());
        }
        let mut vectors = Vec::with_capacity(data.len());
        for entry in data {
            let vector = self.decode_vector(entry)?;
            vectors.push(vector);
        }
        self.health.record_success();
        Ok(vectors)
    }

    fn decode_vector(&self, entry: &Value) -> Result<Vec<f32>, String> {
        let encoded = entry
            .get("embedding")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                self.health.record_failure(FailureKind::InvalidResponse);
                "embedding is not base64".to_string()
            })?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| {
                self.health.record_failure(FailureKind::InvalidResponse);
                "invalid embedding base64".to_string()
            })?;
        if bytes.len() != self.dim * 4 {
            self.health.record_failure(FailureKind::InvalidResponse);
            return Err(format!(
                "embedding dimension mismatch: configured {}, got {}",
                self.dim,
                bytes.len() / 4
            ));
        }
        let values = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect::<Vec<_>>();
        let norm = values
            .iter()
            .map(|value| f64::from(*value) * f64::from(*value))
            .sum::<f64>()
            .sqrt();
        if !values.iter().all(|value| value.is_finite()) || !norm.is_finite() || norm == 0.0 {
            self.health.record_failure(FailureKind::InvalidResponse);
            return Err("embedding contains non-finite values".to_string());
        }
        // L2 normalisation is a property of the deployed model (3.2/4.8); a
        // badly diverged vector means the wrong model or a broken service.
        if (norm - 1.0).abs() > 0.02 {
            self.health.record_failure(FailureKind::InvalidResponse);
            return Err(format!("embedding L2 norm {norm:.3} is not ~1"));
        }
        Ok(values)
    }
}

// ────────────────────────────────────────────────────────────
// Qdrant client
// ────────────────────────────────────────────────────────────

impl QdrantClient {
    fn install_probe(&self) {
        let client = self.client.clone();
        let url = self.url.clone();
        let api_key = self.api_key.clone();
        self.health.set_probe(Some(Arc::new(move || {
            let client = client.clone();
            let url = url.clone();
            let api_key = api_key.clone();
            Box::pin(async move {
                let mut request = client.get(format!("{url}/healthz"));
                if let Some(key) = api_key.as_deref() {
                    request = request.header("api-key", key);
                }
                let response = request.send().await.map_err(|_| ())?;
                response.status().is_success().then_some(()).ok_or(())
            })
        })));
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut request = self.client.request(method, format!("{}{}", self.url, path));
        if let Some(key) = self.api_key.as_deref() {
            request = request.header("api-key", key);
        }
        request
    }

    /// Send a request, retrying one transport-level failure. Every Qdrant call
    /// nyan makes is idempotent (ensure/upsert/delete/search), and a stale
    /// pooled socket otherwise fails a whole sync pass as "error sending
    /// request" (live 2026-10-30 through EasyTier). The timeout is per call:
    /// the query path keeps the short budget while sync batches pass their own.
    async fn send_with_retry(
        &self,
        timeout: Duration,
        build: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, String> {
        match tokio::time::timeout(timeout, build().send()).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(error)) if !error.is_timeout() => {
                match tokio::time::timeout(timeout, build().send()).await {
                    Ok(result) => result.map_err(|error| error.to_string()),
                    Err(_) => Err("Qdrant request timed out".to_string()),
                }
            }
            Ok(Err(error)) => Err(error.to_string()),
            Err(_) => Err("Qdrant request timed out".to_string()),
        }
    }

    async fn upsert(
        &self,
        collection: &str,
        points: Vec<Value>,
        timeout: Duration,
    ) -> Result<(), String> {
        match self.health.call_permit() {
            CallPermit::Skipped => return Err("Qdrant service unreachable".to_string()),
            CallPermit::Probe => {
                self.health.spawn_probe();
                return Err("Qdrant service unreachable".to_string());
            }
            CallPermit::Allowed => {}
        }
        // The collection is created once at sync entry and by the first
        // upsert; re-checking it per batch would add a full RTT (§4.8.1).
        // The points move into the closure; `send_with_retry` may build twice.
        let points = std::sync::Arc::new(points);
        let response = self
            .send_with_retry(timeout, || {
                self.request(
                    reqwest::Method::PUT,
                    &format!("/collections/{collection}/points?wait=false"),
                )
                .json(&json!({"points": points.as_ref()}))
            })
            .await
            .inspect_err(|_error| {
                self.health.record_failure(FailureKind::Unreachable);
            })?;
        if !response.status().is_success() {
            self.health.record_failure(FailureKind::Unreachable);
            return Err(format!("Qdrant upsert HTTP {}", response.status()));
        }
        self.health.record_success();
        Ok(())
    }

    async fn ensure_collection(
        &self,
        collection: &str,
        dimension: usize,
        timeout: Duration,
    ) -> Result<(), String> {
        let response = self
            .send_with_retry(timeout, || {
                self.request(reqwest::Method::PUT, &format!("/collections/{collection}"))
                    .json(&json!({
                        "vectors": {"size": dimension, "distance": "Cosine"}
                    }))
            })
            .await?;
        if response.status().is_success() || response.status().as_u16() == 409 {
            Ok(())
        } else if response.status().as_u16() == 400 {
            // Collection exists with a different vector size: the generation
            // should have changed, so this is a named configuration error.
            Err(format!(
                "Qdrant collection {collection} exists with an incompatible vector size"
            ))
        } else {
            Err(format!("Qdrant collection HTTP {}", response.status()))
        }
    }

    async fn delete_collection(&self, collection: &str, timeout: Duration) -> Result<(), String> {
        let response = self
            .send_with_retry(timeout, || {
                self.request(
                    reqwest::Method::DELETE,
                    &format!("/collections/{collection}"),
                )
            })
            .await?;
        if response.status().is_success() || response.status().as_u16() == 404 {
            Ok(())
        } else {
            Err(format!("Qdrant delete HTTP {}", response.status()))
        }
    }

    async fn delete_path(
        &self,
        collection: &str,
        path: &str,
        timeout: Duration,
    ) -> Result<(), String> {
        let response = self
            .send_with_retry(timeout, || {
                self.request(
                    reqwest::Method::POST,
                    &format!("/collections/{collection}/points/delete?wait=true"),
                )
                .json(&json!({
                    "filter": {"must": [{"key": "path", "match": {"value": path}}]}
                }))
            })
            .await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(format!("Qdrant delete points HTTP {}", response.status()))
        }
    }

    async fn search(
        &self,
        collection: &str,
        vector: &[f32],
        limit: usize,
    ) -> Result<Vec<Value>, String> {
        match self.health.call_permit() {
            CallPermit::Skipped => return Err("Qdrant service unreachable".to_string()),
            CallPermit::Probe => {
                self.health.spawn_probe();
                return Err("Qdrant service unreachable".to_string());
            }
            CallPermit::Allowed => {}
        }
        let response = self
            .send_with_retry(self.query_timeout, || {
                self.request(
                    reqwest::Method::POST,
                    &format!("/collections/{collection}/points/search"),
                )
                .json(&json!({
                    "vector": vector,
                    "limit": limit,
                    "with_payload": true,
                }))
            })
            .await
            .inspect_err(|_error| {
                self.health.record_failure(FailureKind::Unreachable);
            })?;
        if response.status().as_u16() == 404 {
            // The collection does not exist yet: first sync has not finished.
            return Ok(Vec::new());
        }
        if !response.status().is_success() {
            self.health.record_failure(FailureKind::Unreachable);
            return Err(format!("Qdrant HTTP {}", response.status()));
        }
        let body: Value = response.json().await.map_err(|error| {
            self.health.record_failure(FailureKind::InvalidResponse);
            error.to_string()
        })?;
        self.health.record_success();
        Ok(body
            .get("result")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }
}

fn read_key(path: Option<&str>) -> anyhow::Result<Option<String>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let path = crate::utils::path::expand_path(path, None).map_err(anyhow::Error::msg)?;
    if !path.is_file() {
        return Ok(None);
    }
    let value = std::fs::read_to_string(path)?.trim().to_string();
    Ok((!value.is_empty()).then_some(value))
}

pub fn worktree_id(root: &Path) -> String {
    let machine_source = std::fs::read_to_string("/etc/machine-id")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| crate::utils::process_env::var("HOSTNAME"))
        .unwrap_or_else(|| "unknown-machine".to_string());
    let machine = short_hash(machine_source.as_bytes(), 12);
    let path = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    format!(
        "{}-{}",
        machine,
        short_hash(path.to_string_lossy().as_bytes(), 16)
    )
}

fn generation(client: &EmbeddingClient, worktree_id: &str) -> String {
    let value = serde_json::json!({
        "embeddingModelId": client.model,
        "embeddingDim": client.dim,
        "normalize": true,
        "precision": "float32",
        "taskPrefix": {"query": QUERY_PREFIX, "document": ""},
        "queryPreprocessVersion": "v1",
        "documentPreprocessVersion": "v1",
        "chunkerVersion": CHUNKER_VERSION,
        "indexSchemaVersion": GENERATION_VERSION,
        "worktreeId": worktree_id,
    });
    format!("semgen-{}", short_hash(value.to_string().as_bytes(), 32))
}

fn short_hash(bytes: &[u8], length: usize) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
        .chars()
        .take(length)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Local stand-in for corp-pc's vLLM pair and nyan-ops' Qdrant. Records
    /// every request line so tests can assert wire shape, not just results.
    struct MockServices {
        base_url: String,
        requests: Arc<Mutex<Vec<(String, String)>>>,
        dim: usize,
        model: String,
    }

    impl MockServices {
        async fn start(dim: usize, model: &str, search_payload: Value) -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base_url = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let state = Arc::new((
                requests.clone(),
                dim,
                model.to_string(),
                search_payload.clone(),
            ));
            tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        break;
                    };
                    let state = state.clone();
                    tokio::spawn(async move {
                        let _ = serve_one(stream, state).await;
                    });
                }
            });
            Self {
                base_url,
                requests,
                dim,
                model: model.to_string(),
            }
        }

        fn requests(&self) -> Vec<(String, String)> {
            self.requests.lock().unwrap().clone()
        }

        fn settings(&self) -> SemanticSettings {
            SemanticSettings {
                embedding: Some(crate::utils::settings::nyan::EmbeddingSettings {
                    url: Some(self.base_url.clone()),
                    model: Some(self.model.clone()),
                    dim: Some(self.dim as u32),
                    api_key_file: None,
                }),
                qdrant: Some(crate::utils::settings::nyan::QdrantSettings {
                    url: Some(self.base_url.clone()),
                    api_key_file: None,
                }),
            }
        }

        fn reset_health(&self) {
            service_health::shared(ServiceKind::Embedding).reset();
            service_health::shared(ServiceKind::Qdrant).reset();
        }
    }

    async fn serve_one(
        mut stream: tokio::net::TcpStream,
        state: Arc<(Arc<Mutex<Vec<(String, String)>>>, usize, String, Value)>,
    ) -> std::io::Result<()> {
        let (requests, dim, model, search_payload) = (&state.0, state.1, &state.2, &state.3);
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        let header_end = loop {
            let read = stream.read(&mut chunk).await?;
            if read == 0 {
                return Ok(());
            }
            buffer.extend_from_slice(&chunk[..read]);
            if let Some(position) = buffer.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                break position + 4;
            }
        };
        let headers = String::from_utf8_lossy(&buffer[..header_end]).to_string();
        let request_line = headers.lines().next().unwrap_or_default().to_string();
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .and_then(|value| value.trim().parse::<usize>().ok())
            })
            .unwrap_or(0);
        let mut body = buffer[header_end..].to_vec();
        while body.len() < content_length {
            let read = stream.read(&mut chunk).await?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read]);
        }
        let body = String::from_utf8_lossy(&body).to_string();
        requests
            .lock()
            .unwrap()
            .push((request_line.clone(), body.clone()));
        let (status, response) = if request_line.starts_with("GET /v1/models") {
            ("200 OK", json!({"data": [{"id": model}]}))
        } else if request_line.starts_with("POST /v1/embeddings") {
            let count = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|value| value.get("input")?.as_array().map(Vec::len))
                .unwrap_or(1);
            let vector = vec![0.5f32; dim];
            let bytes = vector
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect::<Vec<_>>();
            let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
            let data = (0..count)
                .map(|_| json!({"embedding": encoded, "index": 0}))
                .collect::<Vec<_>>();
            ("200 OK", json!({"data": data}))
        } else if request_line.starts_with("POST /collections/")
            && request_line.contains("/points/search")
        {
            ("200 OK", json!({"result": [search_payload]}))
        } else if request_line.starts_with("GET /healthz") {
            ("200 OK", json!({"title": "qdrant"}))
        } else if request_line.starts_with("GET /collections/") {
            ("200 OK", json!({"result": {"status": "green"}}))
        } else if request_line.starts_with("PUT /collections/") {
            ("200 OK", json!({"result": true}))
        } else {
            ("200 OK", json!({"result": true}))
        };
        let body = response.to_string();
        let reply = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(reply.as_bytes()).await?;
        stream.flush().await
    }

    fn temp_worktree(name: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "nyan-semantic-{name}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let config = root.join("config");
        let project = root.join("project");
        std::fs::create_dir_all(project.join("src")).unwrap();
        (config, project)
    }

    fn open_index(project: &Path) -> LocalIndex {
        LocalIndex::open(
            project,
            1_048_576,
            &crate::tool::ToolPermissionContext::default(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn search_validates_readiness_and_decodes_base64_vectors() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let (config, project) = temp_worktree("search");
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config);
        std::fs::write(project.join("src/a.rs"), "fn needle() {}\n").unwrap();
        let index = open_index(&project);
        let file = index.file("src/a.rs").unwrap();
        let services = MockServices::start(
            4,
            "jina-code-embeddings-1.5b",
            json!({
                "id": "1",
                "score": 0.9,
                "payload": {
                    "path": "src/a.rs",
                    "startLine": 1,
                    "endLine": 1,
                    "fileHashRaw": file.file_hash_raw,
                    "symbol": "needle",
                }
            }),
        )
        .await;
        services.reset_health();
        let client = SemanticClient::from_settings(
            &services.settings(),
            &project,
            &crate::utils::settings::nyan::RetrievalBudgetSettings::default(),
        )
        .unwrap();
        let result = client
            .search(
                &index,
                &SearchRequest::new("needle"),
                Duration::from_secs(5),
            )
            .await;
        assert_eq!(result.status, BranchStatus::Ok);
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.candidates[0].path, "src/a.rs");
        assert_eq!(result.candidates[0].symbol.as_deref(), Some("needle"));
        let requests = services.requests();
        assert!(
            requests
                .iter()
                .any(|(line, _)| line.starts_with("GET /v1/models")),
            "readiness must check the model id: {requests:?}"
        );
        let embedding = requests
            .iter()
            .find(|(line, _)| line.starts_with("POST /v1/embeddings"))
            .expect("embedding request");
        assert!(
            embedding.1.contains("\"encoding_format\":\"base64\""),
            "base64 wire format: {}",
            embedding.1
        );
        assert!(embedding.1.contains(QUERY_PREFIX.trim()));
        let _ = std::fs::remove_dir_all(project.parent().unwrap());
    }

    #[tokio::test]
    async fn readiness_rejects_a_wrong_model_and_wrong_dimension() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let (config, project) = temp_worktree("readiness");
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config);
        std::fs::write(project.join("src/a.rs"), "fn needle() {}\n").unwrap();
        let index = open_index(&project);
        let budget = crate::utils::settings::nyan::RetrievalBudgetSettings::default();
        let payload = json!({"id": "1", "score": 0.5, "payload": {"path": "src/a.rs", "startLine": 1, "endLine": 1, "fileHashRaw": "x"}});

        // Wrong served model: the service answers 200 for a different id.
        let wrong_model = MockServices::start(4, "some-other-model", payload.clone()).await;
        wrong_model.reset_health();
        let mut settings = wrong_model.settings();
        settings.embedding.as_mut().unwrap().model = Some("jina-code-embeddings-1.5b".to_string());
        let client = SemanticClient::from_settings(&settings, &project, &budget).unwrap();
        let result = client
            .search(
                &index,
                &SearchRequest::new("needle"),
                Duration::from_secs(5),
            )
            .await;
        assert!(
            matches!(&result.status, BranchStatus::Unavailable { reason } if reason.contains("not served")),
            "{:?}",
            result.status
        );

        // Right model name, wrong dimension: the readiness probe passes, the
        // embedding response is rejected as invalid rather than truncated.
        service_health::shared(ServiceKind::Embedding).reset();
        let wrong_dim = MockServices::start(3, "jina-code-embeddings-1.5b", payload).await;
        wrong_dim.reset_health();
        let mut settings = wrong_dim.settings();
        // Configure the client for 4 dims while the mock serves 3.
        settings.embedding.as_mut().unwrap().dim = Some(4);
        let client = SemanticClient::from_settings(&settings, &project, &budget).unwrap();
        let result = client
            .search(
                &index,
                &SearchRequest::new("needle"),
                Duration::from_secs(5),
            )
            .await;
        assert!(
            matches!(&result.status, BranchStatus::Unavailable { reason } if reason.contains("dimension mismatch")),
            "{:?}",
            result.status
        );
        let _ = std::fs::remove_dir_all(project.parent().unwrap());
    }

    #[tokio::test]
    async fn sync_embeds_pending_chunks_and_never_sends_source_text_to_qdrant() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let (config, project) = temp_worktree("sync");
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config);
        let secret_free = "fn needle() {\n    let marker_value = 1;\n}\n";
        std::fs::write(project.join("src/a.rs"), secret_free).unwrap();
        std::fs::write(project.join("src/b.rs"), "fn second() { let other = 2; }\n").unwrap();
        let index = open_index(&project);
        let services = MockServices::start(
            4,
            "jina-code-embeddings-1.5b",
            json!({"id": "1", "score": 0.5, "payload": {}}),
        )
        .await;
        services.reset_health();
        let client = SemanticClient::from_settings(
            &services.settings(),
            &project,
            &crate::utils::settings::nyan::RetrievalBudgetSettings::default(),
        )
        .unwrap();
        client.queue_changed(&index).unwrap();
        client.ensure_initial_pending(&index).unwrap();
        assert_eq!(SemanticClient::pending_count(&index), 2);
        let report = client
            .sync_pending(
                &index,
                &crate::utils::settings::nyan::RetrievalSyncSettings::default(),
            )
            .await;
        assert_eq!(report.status, BranchStatus::Ok, "{report:?}");
        assert_eq!(report.pending_files, 0);
        assert_eq!(SemanticClient::pending_count(&index), 0);
        let requests = services.requests();
        let embeddings = requests
            .iter()
            .filter(|(line, _)| line.starts_with("POST /v1/embeddings"))
            .collect::<Vec<_>>();
        assert_eq!(
            embeddings.len(),
            1,
            "small files should share one embedding batch: {requests:?}"
        );
        let embedding_body: serde_json::Value = serde_json::from_str(&embeddings[0].1).unwrap();
        assert_eq!(embedding_body["input"].as_array().unwrap().len(), 2);
        let upserts = requests
            .iter()
            .filter(|(line, _)| line.starts_with("PUT /collections/") && line.contains("/points"))
            .collect::<Vec<_>>();
        assert_eq!(
            upserts.len(),
            1,
            "small files should share one Qdrant upsert: {requests:?}"
        );
        for (_, body) in upserts {
            assert!(!body.contains("marker_value"), "source text leaked: {body}");
            assert!(body.contains("fileHashRaw"));
            assert!(body.contains("chunkId"));
        }
        // The ledger records the collection for later pruning.
        let ledger = read_ledger(index.storage());
        assert!(
            ledger
                .collections
                .iter()
                .any(|entry| entry.collection == client.collection())
        );
        let _ = std::fs::remove_dir_all(project.parent().unwrap());
    }

    #[tokio::test]
    async fn prune_deletes_only_this_machines_replaced_collections() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let (config, project) = temp_worktree("prune");
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config);
        std::fs::write(project.join("src/a.rs"), "fn needle() {}\n").unwrap();
        let index = open_index(&project);
        let services = MockServices::start(
            4,
            "jina-code-embeddings-1.5b",
            json!({"id": "1", "score": 0.5, "payload": {}}),
        )
        .await;
        services.reset_health();
        let client = SemanticClient::from_settings(
            &services.settings(),
            &project,
            &crate::utils::settings::nyan::RetrievalBudgetSettings::default(),
        )
        .unwrap();
        let mut ledger = read_ledger(index.storage());
        ledger.collections.push(LedgerEntry {
            collection: "nyan_old_0000000000000000".to_string(),
            generation: "semgen-old".to_string(),
            created_at_ms: 1,
            last_sync_ms: 1,
        });
        ledger.collections.push(LedgerEntry {
            collection: client.collection().to_string(),
            generation: client.generation().to_string(),
            created_at_ms: 1,
            last_sync_ms: unix_now_ms(),
        });
        write_ledger(index.storage(), &ledger).unwrap();
        let deleted = client
            .prune_replaced_collections(&index)
            .await
            .expect("prune");
        assert_eq!(deleted, vec!["nyan_old_0000000000000000".to_string()]);
        assert!(
            services
                .requests()
                .iter()
                .any(|(line, _)| line.starts_with("DELETE /collections/nyan_old_0000000000000000"))
        );
        let remaining = read_ledger(index.storage());
        assert_eq!(remaining.collections.len(), 1);
        assert_eq!(remaining.collections[0].collection, client.collection());
        let _ = std::fs::remove_dir_all(project.parent().unwrap());
    }

    #[test]
    fn worktree_id_is_machine_and_path_scoped_without_raw_machine_id() {
        let id = worktree_id(Path::new("/tmp/project"));
        assert!(
            id.split('-')
                .all(|part| part.chars().all(|character| character.is_ascii_hexdigit()))
        );
        assert_eq!(id.len(), 12 + 1 + 16);
    }

    #[test]
    fn generation_uses_documented_query_prefix() {
        assert!(QUERY_PREFIX.starts_with("Find the most relevant code snippet"));
    }

    #[test]
    fn queue_round_trips_and_deduplicates() {
        let root =
            std::env::temp_dir().join(format!("nyan-queue-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("semantic-pending.jsonl");
        let entries = vec![
            PendingEntry {
                path: "a.rs".into(),
                file_hash_raw: "1".into(),
            },
            PendingEntry {
                path: "b.rs".into(),
                file_hash_raw: "2".into(),
            },
        ];
        write_queue(&path, &entries).unwrap();
        let mut read = read_queue(&path);
        assert_eq!(read.len(), 2);
        read.retain(|entry| entry.path != "a.rs");
        write_queue(&path, &read).unwrap();
        assert_eq!(read_queue(&path).len(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn chunk_ids_are_stable_and_distinct() {
        assert_eq!(chunk_id("a.rs", 1, 10), chunk_id("a.rs", 1, 10));
        assert_ne!(chunk_id("a.rs", 1, 10), chunk_id("a.rs", 11, 20));
    }

    #[test]
    fn ledger_round_trips() {
        let root =
            std::env::temp_dir().join(format!("nyan-ledger-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&root).unwrap();
        let mut ledger = read_ledger(&root);
        assert!(ledger.collections.is_empty());
        ledger.collections.push(LedgerEntry {
            collection: "nyan_x_y".into(),
            generation: "semgen-y".into(),
            created_at_ms: 1,
            last_sync_ms: 2,
        });
        write_ledger(&root, &ledger).unwrap();
        assert_eq!(read_ledger(&root).collections.len(), 1);
        let _ = std::fs::remove_dir_all(root);
    }
}
