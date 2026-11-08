//! Project agent-memory snapshot sync helpers.
//!
//! Maps to: CC `tools/AgentTool/agentMemorySnapshot.ts`.

use super::agent_memory::{AgentMemoryScope, get_agent_memory_dir};
use std::fs;
use std::path::{Path, PathBuf};

const SNAPSHOT_BASE: &str = "agent-memory-snapshots";
const SNAPSHOT_JSON: &str = "snapshot.json";
const SYNCED_JSON: &str = ".snapshot-synced.json";

/// Maps to: CC `agentMemorySnapshot.ts` check result `action`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentMemorySnapshotAction {
    None,
    Initialize,
    PromptUpdate,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentMemorySnapshotCheck {
    pub action: AgentMemorySnapshotAction,
    pub snapshot_timestamp: Option<String>,
}

/// CC `:16` / `:22` declare BOTH metadata timestamps as `z.string().min(1)`,
/// not a plain `z.string()`. An empty string fails the object parse, and
/// `readJsonFile` turns any parse failure into `null` (`:49-50`) — so an empty
/// timestamp is indistinguishable from a missing file. Plain serde `String`
/// accepted `""` and let both metas through with a falsy value, which flipped
/// two outcomes; see the tests at the bottom of this file.
///
/// The canonical `z.string().min(1)` semantic lives in the zod port
/// (`crate::utils::zod::Schema::min`, `zod/parse.rs` `too_small`). This field
/// deserializer deliberately stays serde: both meta structs and the generic
/// `read_json_file<T: DeserializeOwned>` entry are serde-derive throughout,
/// and the only observable outcome here is parse-failure → `None` (decided
/// under T5 #161 — converting would trade the derive for a hand projection
/// with no behavioral gain).
fn non_empty_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    let value = String::deserialize(deserializer)?;
    if value.is_empty() {
        // zod/v4 `too_small`: "Too small: expected string to have >=1 characters".
        return Err(serde::de::Error::invalid_length(
            0,
            &"a string with at least 1 character",
        ));
    }
    Ok(value)
}

/// Maps to: CC `agentMemorySnapshot.ts:14-18` `snapshotMetaSchema`.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
struct SnapshotMeta {
    #[serde(rename = "updatedAt", deserialize_with = "non_empty_string")]
    updated_at: String,
}

/// Maps to: CC `agentMemorySnapshot.ts:20-24` `syncedMetaSchema`.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
struct SyncedMeta {
    #[serde(rename = "syncedFrom", deserialize_with = "non_empty_string")]
    synced_from: String,
}

/// Maps to: CC `agentMemorySnapshot.ts#getSnapshotDirForAgent`.
pub fn get_snapshot_dir_for_agent(agent_type: &str, cwd: &Path) -> PathBuf {
    cwd.join(".nyan").join(SNAPSHOT_BASE).join(agent_type)
}

fn get_snapshot_json_path(agent_type: &str, cwd: &Path) -> PathBuf {
    get_snapshot_dir_for_agent(agent_type, cwd).join(SNAPSHOT_JSON)
}

fn get_synced_json_path(agent_type: &str, scope: AgentMemoryScope, cwd: &Path) -> PathBuf {
    get_agent_memory_dir(agent_type, scope, cwd).join(SYNCED_JSON)
}

/// Maps to: CC `agentMemorySnapshot.ts:43-54#readJsonFile` — a read error, a
/// JSON syntax error and a SCHEMA failure all collapse to `null`. The schema
/// half is what the `non_empty_string` field decoders supply here.
fn read_json_file<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn copy_snapshot_to_local(agent_type: &str, scope: AgentMemoryScope, cwd: &Path) {
    let snapshot_mem_dir = get_snapshot_dir_for_agent(agent_type, cwd);
    let local_mem_dir = get_agent_memory_dir(agent_type, scope, cwd);
    let _ = fs::create_dir_all(&local_mem_dir);

    let Ok(entries) = fs::read_dir(&snapshot_mem_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name == SNAPSHOT_JSON {
            continue;
        }
        let Ok(content) = fs::read(&path) else {
            tracing::debug!(
                "Failed to copy snapshot to local agent memory: read {}",
                path.display()
            );
            continue;
        };
        if let Err(error) = fs::write(local_mem_dir.join(name), content) {
            tracing::debug!("Failed to copy snapshot to local agent memory: {error}");
        }
    }
}

fn save_synced_meta(
    agent_type: &str,
    scope: AgentMemoryScope,
    cwd: &Path,
    snapshot_timestamp: &str,
) {
    let synced_path = get_synced_json_path(agent_type, scope, cwd);
    let local_mem_dir = get_agent_memory_dir(agent_type, scope, cwd);
    let _ = fs::create_dir_all(&local_mem_dir);
    let meta = SyncedMeta {
        synced_from: snapshot_timestamp.to_string(),
    };
    match serde_json::to_string(&meta) {
        Ok(json) => {
            if let Err(error) = fs::write(&synced_path, json) {
                tracing::debug!("Failed to save snapshot sync metadata: {error}");
            }
        }
        Err(error) => {
            tracing::debug!("Failed to save snapshot sync metadata: {error}");
        }
    }
}

/// CC `:119` `d.isFile() && d.name.endsWith('.md')` — a CASE-SENSITIVE suffix
/// test on the file name. `NOTES.MD` does not count, and a bare `.md` name
/// does. An earlier port used `extension().eq_ignore_ascii_case("md")`, which
/// diverged on both.
fn is_md_file(path: &Path) -> bool {
    path.is_file()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".md"))
}

fn local_memory_has_md_files(agent_type: &str, scope: AgentMemoryScope, cwd: &Path) -> bool {
    let local_mem_dir = get_agent_memory_dir(agent_type, scope, cwd);
    let Ok(entries) = fs::read_dir(local_mem_dir) else {
        return false;
    };
    entries.flatten().any(|entry| is_md_file(&entry.path()))
}

/// Maps to: CC `agentMemorySnapshot.ts#checkAgentMemorySnapshot`.
pub fn check_agent_memory_snapshot(
    agent_type: &str,
    scope: AgentMemoryScope,
    cwd: &Path,
) -> AgentMemorySnapshotCheck {
    let Some(snapshot_meta) =
        read_json_file::<SnapshotMeta>(&get_snapshot_json_path(agent_type, cwd))
    else {
        return AgentMemorySnapshotCheck {
            action: AgentMemorySnapshotAction::None,
            snapshot_timestamp: None,
        };
    };

    if !local_memory_has_md_files(agent_type, scope, cwd) {
        return AgentMemorySnapshotCheck {
            action: AgentMemorySnapshotAction::Initialize,
            snapshot_timestamp: Some(snapshot_meta.updated_at),
        };
    }

    let synced_meta = read_json_file::<SyncedMeta>(&get_synced_json_path(agent_type, scope, cwd));
    // CC `:133-135`: `!syncedMeta || new Date(a) > new Date(b)`. An invalid
    // (or empty) timestamp makes the Date NaN, and any comparison against NaN
    // is FALSE — so a malformed pair never prompts an update. An earlier port
    // fell back to lexicographic string comparison, which invented an
    // ordering CC does not have. Residual deviation: chrono parses RFC 3339
    // where JS `new Date` accepts more shapes; both sides WRITE the value via
    // toISOString/to_rfc3339, so non-RFC values only arise from hand-edited
    // files, and those land on the no-update arm here.
    let needs_update = match synced_meta {
        None => true,
        Some(synced) => match (
            chrono_parse(&snapshot_meta.updated_at),
            chrono_parse(&synced.synced_from),
        ) {
            (Some(snap), Some(synced_at)) => snap > synced_at,
            _ => false,
        },
    };

    if needs_update {
        AgentMemorySnapshotCheck {
            action: AgentMemorySnapshotAction::PromptUpdate,
            snapshot_timestamp: Some(snapshot_meta.updated_at),
        }
    } else {
        AgentMemorySnapshotCheck {
            action: AgentMemorySnapshotAction::None,
            snapshot_timestamp: None,
        }
    }
}

fn chrono_parse(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

/// Maps to: CC `agentMemorySnapshot.ts#initializeFromSnapshot`.
pub fn initialize_from_snapshot(
    agent_type: &str,
    scope: AgentMemoryScope,
    cwd: &Path,
    snapshot_timestamp: &str,
) {
    tracing::debug!("Initializing agent memory for {agent_type} from project snapshot");
    copy_snapshot_to_local(agent_type, scope, cwd);
    save_synced_meta(agent_type, scope, cwd, snapshot_timestamp);
}

/// Maps to: CC `agentMemorySnapshot.ts#replaceFromSnapshot`.
pub fn replace_from_snapshot(
    agent_type: &str,
    scope: AgentMemoryScope,
    cwd: &Path,
    snapshot_timestamp: &str,
) {
    tracing::debug!("Replacing agent memory for {agent_type} with project snapshot");
    let local_mem_dir = get_agent_memory_dir(agent_type, scope, cwd);
    if let Ok(entries) = fs::read_dir(&local_mem_dir) {
        for entry in entries.flatten() {
            // CC `:177` — the same case-sensitive `.md` suffix as the
            // has-local-memory scan; a `NOTES.MD` orphan is deliberately kept.
            let path = entry.path();
            if is_md_file(&path) {
                let _ = fs::remove_file(path);
            }
        }
    }
    copy_snapshot_to_local(agent_type, scope, cwd);
    save_synced_meta(agent_type, scope, cwd, snapshot_timestamp);
}

/// Maps to: CC `agentMemorySnapshot.ts#markSnapshotSynced`.
pub fn mark_snapshot_synced(
    agent_type: &str,
    scope: AgentMemoryScope,
    cwd: &Path,
    snapshot_timestamp: &str,
) {
    save_synced_meta(agent_type, scope, cwd, snapshot_timestamp);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(label: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("nyan-agent-snap-{label}-{stamp}"));
        let _ = fs::create_dir_all(&root);
        root
    }

    #[test]
    fn check_initialize_when_snapshot_exists_without_local_md() {
        let cwd = temp_root("init");
        let agent = "reviewer";
        let snap_dir = get_snapshot_dir_for_agent(agent, &cwd);
        fs::create_dir_all(&snap_dir).unwrap();
        fs::write(
            snap_dir.join(SNAPSHOT_JSON),
            r#"{"updatedAt":"2026-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        fs::write(snap_dir.join("notes.md"), "hello").unwrap();

        let check = check_agent_memory_snapshot(agent, AgentMemoryScope::Project, &cwd);
        assert_eq!(check.action, AgentMemorySnapshotAction::Initialize);
        assert_eq!(
            check.snapshot_timestamp.as_deref(),
            Some("2026-01-01T00:00:00Z")
        );

        initialize_from_snapshot(
            agent,
            AgentMemoryScope::Project,
            &cwd,
            "2026-01-01T00:00:00Z",
        );
        let local = get_agent_memory_dir(agent, AgentMemoryScope::Project, &cwd);
        assert!(local.join("notes.md").is_file());
        assert!(local.join(SYNCED_JSON).is_file());

        let again = check_agent_memory_snapshot(agent, AgentMemoryScope::Project, &cwd);
        assert_eq!(again.action, AgentMemorySnapshotAction::None);

        let _ = fs::remove_dir_all(cwd);
    }

    /// CC `:119` `d.name.endsWith('.md')` is case-sensitive: an upper-case
    /// `NOTES.MD` does NOT count as local memory, so the snapshot still
    /// initializes. An `eq_ignore_ascii_case` port counted it and returned
    /// prompt-update/none instead.
    #[test]
    fn upper_case_md_does_not_count_as_local_memory() {
        let cwd = temp_root("case");
        let agent = "reviewer";
        let snap_dir = get_snapshot_dir_for_agent(agent, &cwd);
        fs::create_dir_all(&snap_dir).unwrap();
        fs::write(
            snap_dir.join(SNAPSHOT_JSON),
            r#"{"updatedAt":"2026-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        let local = get_agent_memory_dir(agent, AgentMemoryScope::Project, &cwd);
        fs::create_dir_all(&local).unwrap();
        fs::write(local.join("NOTES.MD"), "shouting").unwrap();

        let check = check_agent_memory_snapshot(agent, AgentMemoryScope::Project, &cwd);
        assert_eq!(check.action, AgentMemorySnapshotAction::Initialize);

        let _ = fs::remove_dir_all(cwd);
    }

    /// CC `:133-135` compares `new Date(a) > new Date(b)`; an invalid or
    /// empty timestamp is NaN and every comparison against NaN is false — no
    /// update prompt. A lexicographic fallback would order "garbage" strings
    /// and prompt spuriously.
    #[test]
    fn invalid_timestamps_never_prompt_an_update() {
        let cwd = temp_root("nan");
        let agent = "reviewer";
        let snap_dir = get_snapshot_dir_for_agent(agent, &cwd);
        fs::create_dir_all(&snap_dir).unwrap();
        // "z..." sorts lexicographically after any RFC 3339 string, so the
        // string-comparison fallback would report an update here.
        fs::write(
            snap_dir.join(SNAPSHOT_JSON),
            r#"{"updatedAt":"zzz-not-a-date"}"#,
        )
        .unwrap();
        let local = get_agent_memory_dir(agent, AgentMemoryScope::Project, &cwd);
        fs::create_dir_all(&local).unwrap();
        fs::write(local.join("notes.md"), "existing").unwrap();
        fs::write(
            local.join(SYNCED_JSON),
            r#"{"syncedFrom":"2026-01-01T00:00:00Z"}"#,
        )
        .unwrap();

        let check = check_agent_memory_snapshot(agent, AgentMemoryScope::Project, &cwd);
        assert_eq!(
            check.action,
            AgentMemorySnapshotAction::None,
            "NaN comparisons are false in CC — no prompt for malformed timestamps"
        );

        let _ = fs::remove_dir_all(cwd);
    }

    /// CC `:16` `updatedAt: z.string().min(1)`. A `""` (or non-string, or
    /// absent) value fails `snapshotMetaSchema`, `readJsonFile` returns `null`
    /// (`:49-50`), and `:110-112` reports `action: 'none'` — the snapshot is
    /// treated as ABSENT. A plain `String` field accepted `""` and, with no
    /// local memory, walked into `initialize` with an empty timestamp, which
    /// then got written into `.snapshot-synced.json` as the synced marker.
    #[test]
    fn empty_updated_at_makes_the_snapshot_absent() {
        let cwd = temp_root("empty-updated-at");
        let agent = "reviewer";
        let snap_dir = get_snapshot_dir_for_agent(agent, &cwd);
        fs::create_dir_all(&snap_dir).unwrap();
        fs::write(snap_dir.join("notes.md"), "hello").unwrap();

        for malformed in [
            r#"{"updatedAt":""}"#,
            r#"{"updatedAt":123}"#,
            r#"{"updatedAt":null}"#,
            r#"{}"#,
        ] {
            fs::write(snap_dir.join(SNAPSHOT_JSON), malformed).unwrap();
            let check = check_agent_memory_snapshot(agent, AgentMemoryScope::Project, &cwd);
            assert_eq!(
                check,
                AgentMemorySnapshotCheck {
                    action: AgentMemorySnapshotAction::None,
                    snapshot_timestamp: None,
                },
                "a snapshot meta that fails z.string().min(1) is absent, got {check:?} for {malformed}"
            );
        }

        let _ = fs::remove_dir_all(cwd);
    }

    /// CC `:22` `syncedFrom: z.string().min(1)`. A `""` fails
    /// `syncedMetaSchema`, so `syncedMeta` is `null` and `:133-134`'s
    /// `!syncedMeta` short-circuits to `prompt-update` WITHOUT ever comparing
    /// dates. A plain `String` field parsed `""` and went to the comparison,
    /// where `new Date("")` is NaN → false → `none`: the user never got asked.
    #[test]
    fn empty_synced_from_prompts_update_without_comparing_dates() {
        let cwd = temp_root("empty-synced-from");
        let agent = "reviewer";
        let snap_dir = get_snapshot_dir_for_agent(agent, &cwd);
        fs::create_dir_all(&snap_dir).unwrap();
        // Deliberately OLDER than the sync marker would be: if the empty
        // `syncedFrom` were parsed and compared, no arm could prompt.
        fs::write(
            snap_dir.join(SNAPSHOT_JSON),
            r#"{"updatedAt":"2020-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        let local = get_agent_memory_dir(agent, AgentMemoryScope::Project, &cwd);
        fs::create_dir_all(&local).unwrap();
        fs::write(local.join("notes.md"), "existing").unwrap();
        fs::write(local.join(SYNCED_JSON), r#"{"syncedFrom":""}"#).unwrap();

        let check = check_agent_memory_snapshot(agent, AgentMemoryScope::Project, &cwd);
        assert_eq!(
            check,
            AgentMemorySnapshotCheck {
                action: AgentMemorySnapshotAction::PromptUpdate,
                snapshot_timestamp: Some("2020-01-01T00:00:00Z".to_string()),
            },
            "invalid synced metadata is never-synced in CC, not up-to-date"
        );

        // The same shape once the marker is legal again: the snapshot is older
        // than what was synced, so there is nothing to prompt about.
        fs::write(
            local.join(SYNCED_JSON),
            r#"{"syncedFrom":"2026-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!(
            check_agent_memory_snapshot(agent, AgentMemoryScope::Project, &cwd).action,
            AgentMemorySnapshotAction::None
        );

        let _ = fs::remove_dir_all(cwd);
    }
}
