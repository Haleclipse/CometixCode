//! LSP diagnostic registry.
//!
//! Maps to: CC `services/lsp/LSPDiagnosticRegistry.ts`.
//!
//! LSP diagnostics arrive asynchronously from language servers. The registry
//! stores pending diagnostic batches until the next attachment pass can deliver
//! them, deduplicating both within a batch and across turns.

use crate::services::lsp::types::{Diagnostic, DiagnosticFile};
use indexmap::IndexMap;
use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};

const MAX_DIAGNOSTICS_PER_FILE: usize = 10;
const MAX_TOTAL_DIAGNOSTICS: usize = 30;
const MAX_DELIVERED_FILES: usize = 500;

/// Maps to: CC `PendingLSPDiagnostic`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingLspDiagnostic {
    pub server_name: String,
    pub files: Vec<DiagnosticFile>,
    pub timestamp: i64,
    pub attachment_sent: bool,
}

/// Maps to the item returned from CC `checkForLSPDiagnostics()`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LspDiagnosticDelivery {
    pub server_name: String,
    pub files: Vec<DiagnosticFile>,
}

#[derive(Default)]
struct LspDiagnosticRegistryState {
    /// Maps to: CC `pendingDiagnostics = new Map()` (`:49`). A `Map`, so
    /// `checkForLSPDiagnostics` iterates in REGISTRATION order — which decides
    /// both which diagnostics survive the 30-total cap and the order of the
    /// `serverName` join. Both are model-facing, so a `HashMap` keyed by a
    /// random UUID made the attachment text vary run to run.
    pending_diagnostics: IndexMap<String, PendingLspDiagnostic>,
    /// Maps to: CC `deliveredDiagnostics = new LRUCache({max: 500})` (`:54`).
    /// `IndexMap` ordered least-recently-used first: reads and writes move the
    /// entry to the back, eviction pops the front. A plain insertion-order
    /// queue evicts a hot file that CC would have retained, and the same
    /// diagnostic is then re-delivered to the model.
    delivered_diagnostics: IndexMap<String, HashSet<String>>,
}

/// `LRUCache.get`/`.set` both refresh recency; this is that refresh.
fn touch_delivered(delivered: &mut IndexMap<String, HashSet<String>>, file_uri: &str) {
    if let Some(index) = delivered.get_index_of(file_uri) {
        let last = delivered.len() - 1;
        if index != last {
            delivered.move_index(index, last);
        }
    }
}

static REGISTRY: LazyLock<Mutex<LspDiagnosticRegistryState>> =
    LazyLock::new(|| Mutex::new(LspDiagnosticRegistryState::default()));

/// Maps to: CC `registerPendingLSPDiagnostic(...)`.
pub fn register_pending_lsp_diagnostic(server_name: impl Into<String>, files: Vec<DiagnosticFile>) {
    let mut state = REGISTRY.lock().unwrap();
    // CC keys by `randomUUID()` too (`:73`); the ordering comes from the `Map`,
    // not from the key.
    let diagnostic_id = uuid::Uuid::new_v4().to_string();
    state.pending_diagnostics.insert(
        diagnostic_id,
        PendingLspDiagnostic {
            server_name: server_name.into(),
            files,
            timestamp: chrono::Utc::now().timestamp_millis(),
            attachment_sent: false,
        },
    );
}

/// Maps to: CC `checkForLSPDiagnostics()`.
pub fn check_for_lsp_diagnostics() -> Vec<LspDiagnosticDelivery> {
    let mut state = REGISTRY.lock().unwrap();
    let mut all_files = Vec::new();
    let mut server_names = Vec::new();
    let mut diagnostics_to_mark = Vec::new();

    for (id, diagnostic) in &state.pending_diagnostics {
        if !diagnostic.attachment_sent {
            all_files.extend(diagnostic.files.clone());
            if !server_names.contains(&diagnostic.server_name) {
                server_names.push(diagnostic.server_name.clone());
            }
            diagnostics_to_mark.push(id.clone());
        }
    }

    if all_files.is_empty() {
        return Vec::new();
    }

    let mut deduped_files = deduplicate_diagnostic_files(&mut state, all_files);

    for id in diagnostics_to_mark {
        if let Some(diagnostic) = state.pending_diagnostics.get_mut(&id) {
            diagnostic.attachment_sent = true;
        }
    }
    state
        .pending_diagnostics
        .retain(|_, diagnostic| !diagnostic.attachment_sent);

    apply_volume_limits(&mut deduped_files);
    track_delivered_diagnostics(&mut state, &deduped_files);

    let final_count = deduped_files
        .iter()
        .map(|file| file.diagnostics.len())
        .sum::<usize>();
    if final_count == 0 {
        return Vec::new();
    }

    vec![LspDiagnosticDelivery {
        server_name: server_names.join(", "),
        files: deduped_files,
    }]
}

/// Maps to: CC `clearAllLSPDiagnostics()`.
pub fn clear_all_lsp_diagnostics() {
    REGISTRY.lock().unwrap().pending_diagnostics.clear();
}

/// Maps to: CC `resetAllLSPDiagnosticState()`.
pub fn reset_all_lsp_diagnostic_state() {
    let mut state = REGISTRY.lock().unwrap();
    state.pending_diagnostics.clear();
    state.delivered_diagnostics.clear();
}

/// Maps to: CC `clearDeliveredDiagnosticsForFile(fileUri)`.
pub fn clear_delivered_diagnostics_for_file(file_uri: &str) {
    let mut state = REGISTRY.lock().unwrap();
    state.delivered_diagnostics.shift_remove(file_uri);
}

/// Maps to: CC `getPendingLSPDiagnosticCount()`.
pub fn get_pending_lsp_diagnostic_count() -> usize {
    REGISTRY.lock().unwrap().pending_diagnostics.len()
}

fn severity_to_number(severity: Option<&str>) -> u8 {
    match severity {
        Some("Error") => 1,
        Some("Warning") => 2,
        Some("Info") => 3,
        Some("Hint") => 4,
        _ => 4,
    }
}

/// Maps to: CC `createDiagnosticKey(...)`.
fn create_diagnostic_key(diag: &Diagnostic) -> String {
    serde_json::json!({
        "message": diag.message,
        "severity": diag.severity,
        "range": diag.range,
        "source": diag.source,
        "code": diag.code,
    })
    .to_string()
}

/// Maps to: CC `deduplicateDiagnosticFiles(...)`.
fn deduplicate_diagnostic_files(
    state: &mut LspDiagnosticRegistryState,
    all_files: Vec<DiagnosticFile>,
) -> Vec<DiagnosticFile> {
    let mut file_map: HashMap<String, HashSet<String>> = HashMap::new();
    let mut deduped_files: Vec<DiagnosticFile> = Vec::new();
    let mut file_index: HashMap<String, usize> = HashMap::new();

    for file in all_files {
        let idx = match file_index.get(&file.uri).copied() {
            Some(idx) => idx,
            None => {
                let idx = deduped_files.len();
                file_index.insert(file.uri.clone(), idx);
                file_map.insert(file.uri.clone(), HashSet::new());
                deduped_files.push(DiagnosticFile {
                    uri: file.uri.clone(),
                    diagnostics: Vec::new(),
                });
                idx
            }
        };

        let seen = file_map.entry(file.uri.clone()).or_default();
        // Maps to: CC `:153` `deliveredDiagnostics.get(file.uri)` — an
        // LRUCache read, so it refreshes recency.
        touch_delivered(&mut state.delivered_diagnostics, &file.uri);
        let previously_delivered = state
            .delivered_diagnostics
            .get(&file.uri)
            .cloned()
            .unwrap_or_default();

        for diag in file.diagnostics {
            let key = create_diagnostic_key(&diag);
            if seen.contains(&key) || previously_delivered.contains(&key) {
                continue;
            }
            seen.insert(key);
            deduped_files[idx].diagnostics.push(diag);
        }
    }

    deduped_files
        .into_iter()
        .filter(|file| !file.diagnostics.is_empty())
        .collect()
}

fn apply_volume_limits(files: &mut Vec<DiagnosticFile>) {
    let mut total = 0usize;
    for file in files.iter_mut() {
        file.diagnostics
            .sort_by_key(|diag| severity_to_number(Some(&diag.severity)));
        if file.diagnostics.len() > MAX_DIAGNOSTICS_PER_FILE {
            file.diagnostics.truncate(MAX_DIAGNOSTICS_PER_FILE);
        }
        let remaining = MAX_TOTAL_DIAGNOSTICS.saturating_sub(total);
        if file.diagnostics.len() > remaining {
            file.diagnostics.truncate(remaining);
        }
        total += file.diagnostics.len();
    }
    files.retain(|file| !file.diagnostics.is_empty());
}

fn track_delivered_diagnostics(state: &mut LspDiagnosticRegistryState, files: &[DiagnosticFile]) {
    for file in files {
        // Maps to: CC `:292-295` — `has` then `set`/`get`, i.e. the entry ends
        // up most-recently-used either way.
        touch_delivered(&mut state.delivered_diagnostics, &file.uri);
        let delivered = state
            .delivered_diagnostics
            .entry(file.uri.clone())
            .or_default();
        for diag in &file.diagnostics {
            delivered.insert(create_diagnostic_key(diag));
        }
        while state.delivered_diagnostics.len() > MAX_DELIVERED_FILES {
            state.delivered_diagnostics.shift_remove_index(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::lsp::types::{DiagnosticPosition, DiagnosticRange};
    use crate::utils::test_env::TEST_ENV_LOCK;

    fn diag(message: &str, severity: &str, line: u32) -> Diagnostic {
        Diagnostic {
            message: message.to_string(),
            severity: severity.to_string(),
            range: DiagnosticRange {
                start: DiagnosticPosition { line, character: 0 },
                end: DiagnosticPosition { line, character: 1 },
            },
            source: Some("rust-analyzer".to_string()),
            code: None,
        }
    }

    fn file(uri: &str, diagnostics: Vec<Diagnostic>) -> DiagnosticFile {
        DiagnosticFile {
            uri: uri.to_string(),
            diagnostics,
        }
    }

    #[test]
    fn lsp_registry_deduplicates_within_batch_and_across_turns() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        reset_all_lsp_diagnostic_state();
        let duplicate = diag("same", "Warning", 1);
        register_pending_lsp_diagnostic(
            "rust",
            vec![file("file:///a.rs", vec![duplicate.clone(), duplicate])],
        );

        let first = check_for_lsp_diagnostics();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].server_name, "rust");
        assert_eq!(first[0].files[0].diagnostics.len(), 1);

        register_pending_lsp_diagnostic(
            "rust",
            vec![file("file:///a.rs", vec![diag("same", "Warning", 1)])],
        );
        assert!(check_for_lsp_diagnostics().is_empty());

        clear_delivered_diagnostics_for_file("file:///a.rs");
        register_pending_lsp_diagnostic(
            "rust",
            vec![file("file:///a.rs", vec![diag("same", "Warning", 1)])],
        );
        assert_eq!(check_for_lsp_diagnostics()[0].files[0].diagnostics.len(), 1);
        reset_all_lsp_diagnostic_state();
    }

    #[test]
    fn lsp_registry_sorts_by_severity_and_caps_volume_like_official() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        reset_all_lsp_diagnostic_state();
        let mut diagnostics = Vec::new();
        for i in 0..15 {
            diagnostics.push(diag(&format!("hint-{i}"), "Hint", i));
        }
        diagnostics.push(diag("error", "Error", 99));
        diagnostics.push(diag("warning", "Warning", 100));
        register_pending_lsp_diagnostic("server", vec![file("file:///large.rs", diagnostics)]);

        let delivered = check_for_lsp_diagnostics();
        let diagnostics = &delivered[0].files[0].diagnostics;
        assert_eq!(diagnostics.len(), MAX_DIAGNOSTICS_PER_FILE);
        assert_eq!(diagnostics[0].message, "error");
        assert_eq!(diagnostics[1].message, "warning");
        reset_all_lsp_diagnostic_state();
    }

    /// M4: CC's `pendingDiagnostics` is a `Map`, so `checkForLSPDiagnostics`
    /// walks registrations in order (`LSPDiagnosticRegistry.ts:49`, `:206`) and
    /// the `MAX_TOTAL_DIAGNOSTICS = 30` cap (`:272-276`) therefore keeps the
    /// FIRST 30. Keyed by a random UUID in a `HashMap`, which 30 survived — and
    /// the `serverName` join — varied per run, and both reach the model.
    #[test]
    fn registration_order_decides_the_total_cap_and_server_name_join() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        reset_all_lsp_diagnostic_state();

        for server in ["alpha", "zeta", "mid"] {
            let diagnostics = (0..10)
                .map(|index| diag(&format!("{server}-{index}"), "Error", index))
                .collect::<Vec<_>>();
            register_pending_lsp_diagnostic(
                server,
                vec![file(&format!("file:///{server}.rs"), diagnostics)],
            );
        }
        // One more registration past the 30-diagnostic budget.
        register_pending_lsp_diagnostic(
            "omega",
            vec![file("file:///omega.rs", vec![diag("omega-0", "Error", 0)])],
        );

        let delivered = check_for_lsp_diagnostics();
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].server_name, "alpha, zeta, mid, omega");
        assert_eq!(
            delivered[0]
                .files
                .iter()
                .map(|file| file.uri.as_str())
                .collect::<Vec<_>>(),
            vec!["file:///alpha.rs", "file:///zeta.rs", "file:///mid.rs"],
            "the cap must drop the LAST registration, not an arbitrary one"
        );
        assert_eq!(
            delivered[0]
                .files
                .iter()
                .map(|file| file.diagnostics.len())
                .sum::<usize>(),
            MAX_TOTAL_DIAGNOSTICS
        );
        reset_all_lsp_diagnostic_state();
    }

    /// M5: CC tracks delivered files in an `LRUCache({max: 500})` whose `get`
    /// refreshes recency (`:54`, `:153`). Under the old FIFO queue a hot file
    /// was evicted after 500 distinct files even though it had been read every
    /// turn, and its diagnostics were then re-delivered to the model.
    #[test]
    fn delivered_dedup_evicts_least_recently_used_like_the_official_lru() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        reset_all_lsp_diagnostic_state();

        let hot = "file:///hot.rs";
        register_pending_lsp_diagnostic("s", vec![file(hot, vec![diag("hot", "Error", 1)])]);
        assert_eq!(check_for_lsp_diagnostics().len(), 1);

        // 499 more distinct files, each pass also re-reading `hot` (which is
        // what `deduplicateDiagnosticFiles` does every turn) so it stays the
        // most-recently-used entry.
        for index in 0..MAX_DELIVERED_FILES - 1 {
            register_pending_lsp_diagnostic(
                "s",
                vec![
                    file(hot, vec![diag("hot", "Error", 1)]),
                    file(
                        &format!("file:///cold-{index}.rs"),
                        vec![diag("cold", "Error", 1)],
                    ),
                ],
            );
            check_for_lsp_diagnostics();
        }

        // One more file tips the cache past 500 and must evict the OLDEST
        // COLD file, not the continuously-read hot one.
        register_pending_lsp_diagnostic(
            "s",
            vec![file("file:///tip.rs", vec![diag("tip", "Error", 1)])],
        );
        check_for_lsp_diagnostics();

        register_pending_lsp_diagnostic("s", vec![file(hot, vec![diag("hot", "Error", 1)])]);
        assert!(
            check_for_lsp_diagnostics().is_empty(),
            "a hot file must stay deduplicated, as it does under CC's LRU"
        );

        register_pending_lsp_diagnostic(
            "s",
            vec![file("file:///cold-0.rs", vec![diag("cold", "Error", 1)])],
        );
        assert_eq!(
            check_for_lsp_diagnostics().len(),
            1,
            "the least-recently-used file is the one that falls out"
        );
        reset_all_lsp_diagnostic_state();
    }

    #[test]
    fn clear_all_only_clears_pending_not_delivered_dedup() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        reset_all_lsp_diagnostic_state();
        register_pending_lsp_diagnostic(
            "server",
            vec![file("file:///a.rs", vec![diag("a", "Error", 1)])],
        );
        assert_eq!(get_pending_lsp_diagnostic_count(), 1);
        clear_all_lsp_diagnostics();
        assert_eq!(get_pending_lsp_diagnostic_count(), 0);

        register_pending_lsp_diagnostic(
            "server",
            vec![file("file:///a.rs", vec![diag("a", "Error", 1)])],
        );
        assert_eq!(check_for_lsp_diagnostics().len(), 1);
        register_pending_lsp_diagnostic(
            "server",
            vec![file("file:///a.rs", vec![diag("a", "Error", 1)])],
        );
        assert!(check_for_lsp_diagnostics().is_empty());
        reset_all_lsp_diagnostic_state();
    }
}
