//! Transcript logging effect for the REPL conversation.
//!
//! Maps to: CC `hooks/useLogMessages.ts` (whole file), called from
//! `screens/REPL.tsx:5107` as
//! `useLogMessages(messages, messages.length === initialMessages?.length)`.
//!
//! CC records the transcript from ONE place: an effect over the messages
//! array. Every producer — prompt submit, inbox, cron, MCP prompts, the query
//! loop — just appends to that array, and this effect persists whatever is
//! new. Recording at the individual submit sites instead leaves any site that
//! forgets it silently dropping messages from the JSONL.

use std::sync::Arc;

use iocraft::prelude::*;

use crate::types::message::Message;

/// Maps to: CC `useLogMessages.ts:25-32` retained refs.
/// L1 (PORTING.md: React/Ink → iocraft): the synchronous enqueue adapter
/// returns the parent before the next effect, so no async `callSeqRef` is needed.
#[derive(Debug, Default)]
struct LogMessagesCursor {
    /// CC `lastRecordedLengthRef` — messages is append-only between
    /// compactions, so only the new tail is passed on.
    last_recorded_length: usize,
    /// CC `lastParentUuidRef`: the active branch tail, not the last disk row.
    last_parent_uuid: Option<String>,
    /// CC `firstMessageUuidRef` — a changed head means compaction or `/clear`
    /// rebuilt the array, which length alone cannot detect.
    first_message_uuid: Option<String>,
}

/// Where the next transcript write starts, or `None` when there is nothing new.
///
/// Maps to: CC `useLogMessages.ts:40-57` — the `isIncremental` / `startIndex`
/// arithmetic, including the early return when the array has not grown.
fn plan_record_start(
    cursor: &LogMessagesCursor,
    current_first_uuid: Option<&str>,
    len: usize,
) -> Option<usize> {
    // First render leaves `first_message_uuid` unset; compaction and `/clear`
    // change the head uuid. Both take the full-array path, whose own dedup
    // handles the messages-to-keep interleaving.
    let was_first_render = cursor.first_message_uuid.is_none();
    let same_head = current_first_uuid.is_some()
        && !was_first_render
        && current_first_uuid == cursor.first_message_uuid.as_deref();
    let is_incremental = same_head && cursor.last_recorded_length <= len;
    let start = if is_incremental {
        cursor.last_recorded_length
    } else {
        0
    };
    (start != len).then_some(start)
}

/// Records new conversation messages to the session transcript.
///
/// `ignore` mirrors CC's second argument: while the history is still exactly
/// the resumed prefix, nothing needs re-writing.
pub fn use_log_messages(hooks: &mut Hooks, messages: Arc<Vec<Message>>, ignore: bool) {
    // CC `useLogMessages.ts:20` `useAppState(s => s.teamContext)`, passed to
    // `recordTranscript` under the agent-swarms gate (`:70-76`). The session
    // list reads `teamName` back out of the file head to tell team sessions
    // apart, so an unstamped transcript looks like a plain one.
    let team_info = crate::state::app_state::use_app_state(hooks, |state| {
        if !crate::utils::agent_swarms_enabled::is_agent_swarms_enabled() {
            return None;
        }
        state
            .team_context
            .as_ref()
            .map(|context| crate::utils::session_storage::TeamInfo {
                team_name: Some(context.team_name.clone()),
                agent_name: context.self_agent_name.clone(),
            })
    });
    let mut cursor = hooks.use_ref(LogMessagesCursor::default);
    // CC's dependency array is `[messages, ignore, …]` — a new array identity
    // re-runs the effect. `history_state` publishes a fresh `Arc` per update,
    // so pointer identity is the same signal.
    // CC's deps also list the two team fields (`:119`).
    let deps = (
        Arc::as_ptr(&messages) as usize,
        ignore,
        team_info.clone().unwrap_or_default(),
    );
    hooks.use_effect(
        move || {
            if ignore {
                return;
            }
            let current_first_uuid = messages.first().map(|message| message.uuid().to_string());
            // CC :57 — nothing new to write; the cursors stay as they are.
            let Some(start_index) = plan_record_start(
                &cursor.read(),
                current_first_uuid.as_deref(),
                messages.len(),
            ) else {
                return;
            };

            // CC :65 — full-array writes rediscover the retained prefix;
            // incremental writes use the previous effect's parent hint.
            let parent_hint = (start_index != 0)
                .then(|| cursor.read().last_parent_uuid.clone())
                .flatten();
            let last_parent_uuid =
                match crate::utils::session_storage::record_typed_messages_with_team(
                    &messages[start_index..],
                    team_info.as_ref(),
                    parent_hint.as_deref(),
                ) {
                    Ok(parent) => parent.or_else(|| cursor.read().last_parent_uuid.clone()),
                    Err(error) => {
                        crate::utils::debug::log_for_debugging(&format!(
                            "Failed to record conversation messages: {error}"
                        ));
                        cursor.read().last_parent_uuid.clone()
                    }
                };

            // CC :117-118 — unconditional, after the write is dispatched.
            cursor.set(LogMessagesCursor {
                last_recorded_length: messages.len(),
                last_parent_uuid,
                first_message_uuid: current_first_uuid,
            });
        },
        deps,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    /// What the effect does after a write: CC `useLogMessages.ts:117-118`.
    fn advance(cursor: &mut LogMessagesCursor, head: Option<&str>, len: usize) {
        cursor.last_recorded_length = len;
        cursor.first_message_uuid = head.map(str::to_string);
    }

    /// CC `:40-57` on an append-only array — only the new tail is handed over.
    #[test]
    fn incremental_growth_records_only_the_new_tail() {
        let mut cursor = LogMessagesCursor::default();
        assert_eq!(
            plan_record_start(&cursor, Some("u1"), 2),
            Some(0),
            "first render walks the full array"
        );
        advance(&mut cursor, Some("u1"), 2);

        assert_eq!(
            plan_record_start(&cursor, Some("u1"), 4),
            Some(2),
            "only the appended tail is recorded"
        );
    }

    /// CC `:44-47` — a changed head uuid (compaction, `/clear`) forces the full
    /// array even though it grew, because the tail alone is no longer the diff.
    #[test]
    fn changed_head_uuid_rewalks_the_whole_array() {
        let mut cursor = LogMessagesCursor::default();
        advance(&mut cursor, Some("u1"), 2);
        assert_eq!(plan_record_start(&cursor, Some("summary"), 3), Some(0));
    }

    /// CC `:53-56` — same head but a shorter array (tombstone, rewind, snip)
    /// is not incremental either.
    #[test]
    fn same_head_shrink_rewalks_the_whole_array() {
        let mut cursor = LogMessagesCursor::default();
        advance(&mut cursor, Some("u1"), 5);
        assert_eq!(plan_record_start(&cursor, Some("u1"), 3), Some(0));
    }

    /// CC `:57` — an unchanged array writes nothing a second time.
    #[test]
    fn unchanged_array_records_nothing() {
        let mut cursor = LogMessagesCursor::default();
        advance(&mut cursor, Some("u1"), 2);
        assert_eq!(plan_record_start(&cursor, Some("u1"), 2), None);
    }

    /// An empty history has nothing to write, on the first render or later.
    #[test]
    fn empty_history_records_nothing() {
        let cursor = LogMessagesCursor::default();
        assert_eq!(plan_record_start(&cursor, None, 0), None);
    }
    #[derive(Default, Props)]
    struct TranscriptHarnessProps {
        changes: Option<async_channel::Receiver<Vec<Message>>>,
    }

    #[component]
    fn TranscriptHarness(
        props: &TranscriptHarnessProps,
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let mut messages = hooks.use_state(|| Arc::new(Vec::<Message>::new()));
        let receiver = props.changes.clone().unwrap();
        hooks.use_future(async move {
            while let Ok(next) = receiver.recv().await {
                messages.set(Arc::new(next));
            }
        });
        let snapshot = messages.read().clone();
        use_log_messages(&mut hooks, snapshot.clone(), false);
        element! { Text(content: format!("len={} head={} tail={}", snapshot.len(), snapshot.first().map(|m| m.uuid()).unwrap_or("empty"), snapshot.last().map(|m| m.uuid()).unwrap_or("empty"))) }
    }

    /// CC useLogMessages.ts:48-113 + sessionStorage.ts:1420-1448. Drive the
    /// retained production effect, then discard process caches and use the
    /// actual resume chain loader. A raw-writer-only test misses this wiring.
    #[test]
    fn rewind_then_new_message_and_resume_matches_official_parent_chain() {
        use crate::state::app_state::{AppStateProvider, ProviderChildren};
        use crate::utils::session_storage as storage;
        use futures::StreamExt;
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _write = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let fixture = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("rewind-hook-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&fixture).unwrap();
        let session_id = uuid::Uuid::new_v4().to_string();
        let previous_id = crate::bootstrap::state::get_session_id();
        let previous_dir = crate::bootstrap::state::get_session_project_dir();
        crate::bootstrap::state::switch_session(&session_id, Some(fixture.as_path().to_path_buf()));
        storage::clear_session_metadata();
        let user = |text: &str| {
            Message::User(crate::utils::messages::create_user_message(
                text.to_string(),
            ))
        };
        let a = user("keep A");
        let b = user("discard B");
        let c = user("new C");
        let d = user("new root D");
        let e = user("new E");
        let boundary =
            Message::System(crate::types::message::SystemMessage::compact_boundary(None));
        let summary = user("compact summary");
        let f = user("after compact F");
        let (tx, rx) = async_channel::unbounded();
        futures::executor::block_on(async {
            let mut app = element! {
                AppStateProvider(children: ProviderChildren::new(move || element!(TranscriptHarness(changes: Some(rx.clone()))).into_any()))
            };
            let mut frames = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(futures::stream::pending()).with_size(80, 5),
            ));
            // Same-head shrink, then empty rewind, then changed-head compaction
            // with already-recorded messagesToKeep after NEW boundary/summary.
            for messages in [
                vec![a.clone(), b.clone()],
                vec![a.clone()],
                vec![a.clone(), c.clone()],
                vec![],
                vec![d.clone()],
                vec![d.clone(), e.clone()],
                vec![boundary.clone(), summary.clone(), d.clone(), e.clone()],
                vec![
                    boundary.clone(),
                    summary.clone(),
                    d.clone(),
                    e.clone(),
                    f.clone(),
                ],
            ] {
                let tail = messages
                    .last()
                    .map(|m| m.uuid())
                    .unwrap_or("empty")
                    .to_string();
                let marker = format!(
                    "len={} head={}",
                    messages.len(),
                    messages.first().map(|m| m.uuid()).unwrap_or("empty")
                );
                tx.send(messages).await.unwrap();
                let mut seen = false;
                for _ in 0..10 {
                    let frame = crate::utils::race(frames.next(), async {
                        futures_timer::Delay::new(std::time::Duration::from_millis(150)).await;
                        None
                    })
                    .await;
                    let Some(frame) = frame else {
                        break;
                    };
                    if frame.to_string().contains(&marker) {
                        seen = true;
                        break;
                    }
                }
                assert!(seen, "retained hook must render {tail}");
                if tail == c.uuid() || tail == d.uuid() {
                    storage::flush_session_storage().await.unwrap();
                    storage::clear_session_metadata();
                    let loaded = storage::load_session_structured_from_path(
                        &fixture.join(format!("{session_id}.jsonl")),
                    );
                    // CC :1441-1448: an all-recorded rewind prefix returns its tail.
                    if tail == c.uuid() {
                        let chain = storage::build_conversation_chain(&loaded, c.uuid());
                        assert_eq!(
                            chain
                                .iter()
                                .map(|m| m["uuid"].as_str().unwrap())
                                .collect::<Vec<_>>(),
                            vec![a.uuid(), c.uuid()]
                        );
                    } else {
                        assert!(
                            loaded.messages[d.uuid()]["parentUuid"].is_null(),
                            "empty rewind starts a root"
                        );
                    }
                }
            }
            // Poll once more so the final render's effects are settled.
            let _ = crate::utils::race(frames.next(), async {
                futures_timer::Delay::new(std::time::Duration::from_millis(50)).await;
                None
            })
            .await;
            storage::flush_session_storage().await.unwrap();
        });
        let path = fixture.as_path().join(format!("{session_id}.jsonl"));
        storage::clear_session_metadata();
        let loaded = storage::load_session_structured_from_path(&path);
        // CC :1400-1407: old messagesToKeep after a new summary cannot advance parent.
        assert_eq!(loaded.messages[f.uuid()]["parentUuid"], summary.uuid());
        let compact_chain = storage::build_conversation_chain(&loaded, f.uuid());
        assert_eq!(
            compact_chain
                .iter()
                .map(|m| m["uuid"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec![summary.uuid(), f.uuid()]
        );
        // Cached UUIDs include pending writes: replaying the prefix never duplicates it.
        let rows = std::fs::read_to_string(&path).unwrap();
        // Small-file resume starts after a non-preserved compact boundary;
        // verify the boundary itself in the persisted wire, not the pruned loader map.
        let boundary_entry: serde_json::Value = rows
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .find(|entry| entry["uuid"] == boundary.uuid())
            .unwrap();
        assert!(boundary_entry["parentUuid"].is_null());

        assert_eq!(
            rows.lines()
                .filter(|line| line.contains(&format!("\"uuid\":\"{}\"", a.uuid())))
                .count(),
            1
        );
        crate::bootstrap::state::switch_session(previous_id, previous_dir);
        storage::clear_session_metadata();
        std::fs::remove_dir_all(fixture).unwrap();
    }
}
