//! Maps to: CC `commands/branch/branch.ts`.

use crate::bootstrap::state::{get_original_cwd, get_session_id};
use crate::utils::json::parse_jsonl;
use crate::utils::session_storage::{
    SearchSessionsByCustomTitleOptions, get_project_dir, get_transcript_path,
    get_transcript_path_for_session, is_transcript_message, search_sessions_by_custom_title,
};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

/// Native continuation carrier for CC `branch.ts#call`: REPL awaits resume
/// before delivering the command-owned success text through onDone(system).
pub struct PreparedBranch {
    pub target: crate::commands::resume::ResumeTarget,
    pub success_message: String,
}

/// Maps to: CC `commands/branch/branch.ts:222-296#call` preparation through
/// context.resume. The local-JSX transport schedules blocking filesystem work;
/// REPL completes the resume callback and then publishes this result.
pub fn call(args: &str) -> Result<PreparedBranch, String> {
    let title =
        args.trim_matches(|ch: char| (ch.is_whitespace() && ch != '\u{85}') || ch == '\u{feff}');
    let custom_title = (!title.is_empty()).then(|| title.to_string());
    let original_session_id = get_session_id();
    let fork = create_fork(custom_title)?;
    let first_prompt = derive_first_prompt(
        fork.serialized_messages
            .iter()
            .find(|message| message.get("type").and_then(Value::as_str) == Some("user")),
    );
    let effective_title = get_unique_fork_name(fork.title.as_deref().unwrap_or(&first_prompt));
    crate::utils::session_storage::save_custom_title(
        &fork.session_id,
        &effective_title,
        Some(&fork.fork_path),
    )
    .map_err(|error| error.to_string())?;
    // Maps to: CC branch.ts:254-257. Creation is recorded after title
    // persistence and before context.resume, even if that later resume fails.
    crate::services::analytics::log_event(
        "tengu_conversation_forked",
        json!({
            "message_count": fork.serialized_messages.len(),
            "has_custom_title": fork.title.is_some(),
        }),
    );
    let title_info = fork
        .title
        .as_ref()
        .map(|title| format!(" \"{title}\""))
        .unwrap_or_default();
    Ok(PreparedBranch {
        success_message: format!(
            "Branched conversation{title_info}. You are now in the branch.\nTo resume the original: claude -r {original_session_id}"
        ),
        target: crate::commands::resume::ResumeTarget {
            session_id: fork.session_id.clone(),
            project_path: None,
            entries: fork.serialized_messages,
            turn_interruption_state: crate::utils::conversation::TurnInterruptionState::None,
            metadata: crate::commands::resume::ResumeMetadata {
                session_id: Some(fork.session_id),
                custom_title: Some(effective_title),
                full_path: Some(fork.fork_path.to_string_lossy().into_owned()),
                content_replacements: fork.content_replacement_records,
                ..Default::default()
            },
            entrypoint: Some(crate::types::command::ResumeEntrypoint::Fork),
        },
    })
}

/// Return-object carrier for CC `commands/branch/branch.ts#createFork`.
/// Raw entries preserve fields that the native display-message types omit.
#[derive(Clone, Debug)]
pub struct ForkResult {
    pub session_id: String,
    pub title: Option<String>,
    pub fork_path: PathBuf,
    pub serialized_messages: Vec<Value>,
    pub content_replacement_records: Vec<Value>,
}

/// Maps to: CC `commands/branch/branch.ts:38-54#deriveFirstPrompt`.
pub fn derive_first_prompt(first_user_message: Option<&Value>) -> String {
    let content = first_user_message.and_then(|entry| entry.get("message")?.get("content"));
    let raw = content.and_then(|content| {
        content.as_str().or_else(|| {
            content
                .as_array()?
                .iter()
                .find(|block| block.get("type").and_then(Value::as_str) == Some("text"))?
                .get("text")?
                .as_str()
        })
    });
    let Some(raw) = raw.filter(|raw| !raw.is_empty()) else {
        return "Branched conversation".to_string();
    };
    // JS /\s+/ followed by trim: FEFF participates, U+0085 does not.
    // Same ECMAScript whitespace predicate as the existing messages owner.
    let collapsed = raw
        .split(|ch: char| (ch.is_whitespace() && ch != '\u{85}') || ch == '\u{feff}')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if collapsed.is_empty() {
        return "Branched conversation".to_string();
    }
    // slice(0, 100) counts UTF-16 units. Rust String cannot carry the isolated
    // high surrogate produced when this cut bisects an astral character;
    // that existing String boundary projects it to U+FFFD, not a second char.
    String::from_utf16_lossy(&collapsed.encode_utf16().take(100).collect::<Vec<_>>())
}

/// Maps to: CC `commands/branch/branch.ts:61-173#createFork`.
/// Filesystem operations retain source order at the native synchronous storage
/// boundary. The caller's async command transport owns scheduling.
pub fn create_fork(custom_title: Option<String>) -> Result<ForkResult, String> {
    let fork_session_id = uuid::Uuid::new_v4().to_string();
    let original_session_id = get_session_id();
    let project_dir = get_project_dir(&get_original_cwd().to_string_lossy());
    let fork_session_path = get_transcript_path_for_session(&fork_session_id);
    let current_transcript_path = get_transcript_path(None);

    let mut directory = fs::DirBuilder::new();
    directory.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directory.mode(0o700);
    }
    directory
        .create(project_dir)
        .map_err(|error| error.to_string())?;

    let transcript_content =
        fs::read(current_transcript_path).map_err(|_| "No conversation to branch".to_string())?;
    if transcript_content.is_empty() {
        return Err("No conversation to branch".to_string());
    }
    let entries = parse_jsonl(&transcript_content);
    if entries.iter().any(Value::is_null) {
        // The shared native predicate is total, whereas CC dereferences
        // entry.type. Transport Bun's actual exception through call's catch
        // rather than silently dropping a parseable null transcript row.
        return Err("null is not an object (evaluating 'entry.type')".to_string());
    }
    let main_conversation_entries = entries
        .iter()
        .filter(|entry| {
            is_transcript_message(entry)
                && !entry
                    .get("isSidechain")
                    .is_some_and(|sidechain| match sidechain {
                        Value::Null => false,
                        Value::Bool(value) => *value,
                        Value::Number(value) => value.as_f64() != Some(0.0),
                        Value::String(value) => !value.is_empty(),
                        Value::Array(_) | Value::Object(_) => true,
                    })
        })
        .collect::<Vec<_>>();
    let content_replacement_records = entries
        .iter()
        .filter(|entry| {
            entry.get("type").and_then(Value::as_str) == Some("content-replacement")
                && entry.get("sessionId").and_then(Value::as_str)
                    == Some(original_session_id.as_str())
        })
        .flat_map(|entry| {
            // Array.flatMap flattens arrays by one level and retains any
            // other value; well-formed ContentReplacementEntry has an array.
            match entry.get("replacements") {
                Some(Value::Array(records)) => records.clone(),
                Some(record) => vec![record.clone()],
                None => vec![Value::Null],
            }
        })
        .collect::<Vec<_>>();
    if main_conversation_entries.is_empty() {
        return Err("No messages to branch".to_string());
    }

    let mut parent_uuid = Some(Value::Null);
    let mut lines = Vec::new();
    let mut serialized_messages = Vec::new();
    for entry in main_conversation_entries {
        let mut forked_entry = entry.clone();
        forked_entry["sessionId"] = json!(fork_session_id);
        if let Some(parent_uuid) = &parent_uuid {
            forked_entry["parentUuid"] = parent_uuid.clone();
        } else if let Some(object) = forked_entry.as_object_mut() {
            // JSON.stringify omits undefined fields; malformed but parseable
            // entries without uuid must not invent a null next parent.
            object.remove("parentUuid");
        }
        forked_entry["isSidechain"] = Value::Bool(false);
        let mut forked_from = json!({"sessionId": original_session_id});
        if let Some(uuid) = entry.get("uuid") {
            forked_from["messageUuid"] = uuid.clone();
        }
        forked_entry["forkedFrom"] = forked_from;

        // The in-memory resume payload intentionally keeps original parents
        // and all original metadata; only its sessionId changes in CC.
        let mut serialized = entry.clone();
        serialized["sessionId"] = json!(fork_session_id);
        serialized_messages.push(serialized);
        lines.push(serde_json::to_string(&forked_entry).map_err(|error| error.to_string())?);
        if entry.get("type").and_then(Value::as_str) != Some("progress") {
            parent_uuid = entry.get("uuid").cloned();
        }
    }
    if !content_replacement_records.is_empty() {
        lines.push(
            serde_json::to_string(&json!({
                "type": "content-replacement",
                "sessionId": fork_session_id,
                "replacements": content_replacement_records,
            }))
            .map_err(|error| error.to_string())?,
        );
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(&fork_session_path)
        .and_then(|mut file| file.write_all((lines.join("\n") + "\n").as_bytes()))
        .map_err(|error| error.to_string())?;

    Ok(ForkResult {
        session_id: fork_session_id,
        title: custom_title,
        fork_path: fork_session_path,
        serialized_messages,
        content_replacement_records,
    })
}

/// Maps to: CC `commands/branch/branch.ts:179-220#getUniqueForkName`.
pub fn get_unique_fork_name(base_name: &str) -> String {
    let candidate_name = format!("{base_name} (Branch)");
    let existing_with_exact_name = search_sessions_by_custom_title(
        &candidate_name,
        Some(SearchSessionsByCustomTitleOptions {
            exact: true,
            ..Default::default()
        }),
    );
    if existing_with_exact_name.is_empty() {
        return candidate_name;
    }
    let existing_forks = search_sessions_by_custom_title(&format!("{base_name} (Branch"), None);
    let mut used_numbers = HashSet::from([1usize]);
    // regex::escape is the native literal-pattern carrier for escapeRegExp;
    // JS \d is ASCII even though Rust regex's unqualified \d is Unicode.
    let fork_number_pattern = regex::Regex::new(&format!(
        r"^{} \(Branch(?: ([0-9]+))?\)$",
        regex::escape(base_name),
    ))
    .expect("escaped branch title is a valid regular expression");
    for session in existing_forks {
        let Some(title) = session.custom_title else {
            continue;
        };
        let Some(captures) = fork_number_pattern.captures(&title) else {
            continue;
        };
        if let Some(number) = captures.get(1) {
            if let Ok(number) = number.as_str().parse::<usize>() {
                used_numbers.insert(number);
            }
        } else {
            used_numbers.insert(1);
        }
    }
    let mut next_number = 2;
    while used_numbers.contains(&next_number) {
        next_number += 1;
    }
    format!("{base_name} (Branch {next_number})")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::session_storage::{
        TestProjectsDirOverrideGuard, set_test_projects_dir_override,
    };
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    struct BranchFixture {
        root: PathBuf,
        original_cwd: PathBuf,
        original_session_id: String,
        original_session_project: Option<PathBuf>,
        _projects: TestProjectsDirOverrideGuard,
    }

    impl BranchFixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("cometix-branch-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(root.join("working")).unwrap();
            fs::create_dir_all(root.join("active-project")).unwrap();
            let fixture = Self {
                original_cwd: get_original_cwd(),
                original_session_id: get_session_id(),
                original_session_project: crate::bootstrap::state::get_session_project_dir(),
                _projects: set_test_projects_dir_override(root.join("projects")),
                root,
            };
            crate::bootstrap::state::set_original_cwd(fixture.root.join("working"));
            crate::bootstrap::state::switch_session(
                "11111111-1111-4111-8111-111111111111",
                Some(fixture.root.join("active-project")),
            );
            fixture
        }

        fn write_current(&self, entries: &[Value]) {
            let content = entries
                .iter()
                .map(|entry| serde_json::to_string(entry).unwrap())
                .collect::<Vec<_>>()
                .join("\n");
            fs::write(get_transcript_path(None), content + "\n").unwrap();
        }

        fn write_named_session(&self, title: &str) {
            let id = uuid::Uuid::new_v4().to_string();
            let directory = get_project_dir(&get_original_cwd().to_string_lossy());
            fs::create_dir_all(&directory).unwrap();
            let entries = [
                json!({
                    "type":"user", "uuid":uuid::Uuid::new_v4().to_string(),
                    "parentUuid":null,"sessionId":id,"isSidechain":false,
                    "cwd":get_original_cwd(),"timestamp":"2026-09-12T00:00:00Z",
                    "message":{"role":"user","content":"prompt"},
                }),
                json!({"type":"custom-title","sessionId":id,"customTitle":title}),
            ];
            fs::write(
                directory.join(format!("{id}.jsonl")),
                entries
                    .iter()
                    .map(|entry| serde_json::to_string(entry).unwrap())
                    .collect::<Vec<_>>()
                    .join("\n")
                    + "\n",
            )
            .unwrap();
        }
    }

    impl Drop for BranchFixture {
        fn drop(&mut self) {
            crate::bootstrap::state::set_original_cwd(&self.original_cwd);
            crate::bootstrap::state::switch_session(
                &self.original_session_id,
                self.original_session_project.clone(),
            );
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn derive_first_prompt_matches_bun_source_selection_and_whitespace() {
        assert_eq!(derive_first_prompt(None), "Branched conversation");
        for (entry, expected) in [
            (json!({"message":{}}), "Branched conversation"),
            (json!({"message":{"content":""}}), "Branched conversation"),
            (
                json!({"message":{"content":[{"type":"image"},{"type":"text","text":"a\nb"}]}}),
                "a b",
            ),
            (
                json!({"message":{"content":[{"type":"text","text":""},{"type":"text","text":"later"}]}}),
                "Branched conversation",
            ),
            (
                json!({"message":{"content":" \u{feff}\talpha\n  beta\r\u{a0}"}}),
                "alpha beta",
            ),
            (
                json!({"message":{"content":"\u{85}x\u{85}"}}),
                "\u{85}x\u{85}",
            ),
            (
                json!({"message":{"content":"\u{feff}\n\t"}}),
                "Branched conversation",
            ),
        ] {
            assert_eq!(derive_first_prompt(Some(&entry)), expected, "{entry}");
        }
    }

    #[test]
    fn derive_first_prompt_counts_utf16_and_records_isolated_surrogate_boundary() {
        let prefix = "a".repeat(98);
        assert_eq!(
            derive_first_prompt(Some(&json!({"message":{"content":format!("{prefix}😀b")}}))),
            format!("{prefix}😀"),
        );
        // Actual Bun oracle returns 99 `a` + lone D83D. This assertion records
        // the existing native String projection; it is not a Bun parity claim.
        let prefix = "a".repeat(99);
        assert_eq!(
            derive_first_prompt(Some(&json!({"message":{"content":format!("{prefix}😀b")}}))),
            format!("{prefix}\u{fffd}"),
        );
    }

    #[test]
    fn create_fork_matches_bun_raw_metadata_distinct_parents_and_replacements() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let fixture = BranchFixture::new();
        let original_id = get_session_id();
        let entries = vec![
            json!({"type":"user","uuid":"u1","parentUuid":"old-parent","sessionId":original_id,
                "timestamp":"2026-01-01T00:00:00Z","gitBranch":"topic","slug":"old-plan",
                "extra":{"nested":[1,"two"]},"message":{"role":"user","content":"first\nprompt"}}),
            json!({"type":"assistant","uuid":"side","parentUuid":"u1","sessionId":original_id,
                "isSidechain":true,"message":{"role":"assistant","content":[]}}),
            json!({"type":"progress","uuid":"p","parentUuid":"u1","sessionId":original_id}),
            json!({"type":"system","uuid":"boundary","parentUuid":"discarded","sessionId":original_id,
                "subtype":"compact_boundary","compactMetadata":{"trigger":"auto","preTokens":12}}),
            json!({"type":"assistant","uuid":"a1","parentUuid":"u1","sessionId":original_id,
                "isSidechain":false,"message":{"role":"assistant","content":[{"type":"text","text":"answer"}]}}),
            json!({"type":"attachment","uuid":"attach","parentUuid":"side","sessionId":original_id,
                "attachment":{"type":"directory","content":"files"}}),
            json!({"type":"content-replacement","sessionId":original_id,"replacements":[{"toolUseId":"t","preview":"one"}]}),
            json!({"type":"content-replacement","sessionId":"other","replacements":[{"toolUseId":"other","preview":"skip"}]}),
            json!({"type":"content-replacement","sessionId":original_id,"replacements":[{"toolUseId":"t","preview":"two"},{"toolUseId":"u","preview":"three"}]}),
            json!({"type":"custom-title","sessionId":original_id,"customTitle":"old"}),
        ];
        fixture.write_current(&entries);
        let original_path = get_transcript_path(None);
        let original_bytes = fs::read(&original_path).unwrap();
        let fork = create_fork(Some("chosen".into())).unwrap();
        assert_eq!(fork.title.as_deref(), Some("chosen"));
        assert_ne!(fork.session_id, original_id);
        assert_eq!(
            get_session_id(),
            original_id,
            "createFork does not switch sessions"
        );
        assert_eq!(
            fork.fork_path,
            get_transcript_path_for_session(&fork.session_id)
        );
        assert_ne!(fork.fork_path.parent(), original_path.parent());
        assert_eq!(fs::read(original_path).unwrap(), original_bytes);
        let bytes = fs::read(&fork.fork_path).unwrap();
        assert!(bytes.ends_with(b"\n"));
        let disk = parse_jsonl(&bytes);
        assert_eq!(disk.len(), 5);
        assert_eq!(fork.serialized_messages.len(), 4);
        for (index, original_index) in [0, 3, 4, 5].into_iter().enumerate() {
            let mut expected_memory = entries[original_index].clone();
            expected_memory["sessionId"] = json!(fork.session_id);
            assert_eq!(fork.serialized_messages[index], expected_memory);
            let mut expected_disk = expected_memory;
            expected_disk["parentUuid"] = if index == 0 {
                Value::Null
            } else {
                disk[index - 1]["uuid"].clone()
            };
            expected_disk["isSidechain"] = json!(false);
            expected_disk["forkedFrom"] =
                json!({"sessionId":original_id,"messageUuid":entries[original_index]["uuid"]});
            assert_eq!(disk[index], expected_disk);
        }
        let replacements = json!([
            {"toolUseId":"t","preview":"one"},
            {"toolUseId":"t","preview":"two"},
            {"toolUseId":"u","preview":"three"},
        ]);
        assert_eq!(json!(fork.content_replacement_records), replacements);
        assert_eq!(
            disk[4],
            json!({"type":"content-replacement","sessionId":fork.session_id,"replacements":replacements})
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(fork.fork_path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn create_fork_reads_buffer_bom_skips_bad_lines_and_does_not_add_empty_replacements() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _fixture = BranchFixture::new();
        fs::write(
            get_transcript_path(None),
            concat!(
                "\u{feff}{\"type\":\"user\",\"uuid\":\"u\",\"message\":{\"content\":\"before\"}}\n",
                "not-json\n\n",
                "{\"type\":\"system\",\"uuid\":\"b\",\"subtype\":\"compact_boundary\"}\n",
                "{\"type\":\"user\",\"uuid\":\"v\",\"message\":{\"content\":\"after\"}}\n",
            ),
        )
        .unwrap();
        let fork = create_fork(None).unwrap();
        assert!(fork.content_replacement_records.is_empty());
        let disk = parse_jsonl(&fs::read(fork.fork_path).unwrap());
        assert_eq!(disk.len(), 3);
        assert_eq!(
            disk.iter()
                .map(|entry| entry["uuid"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["u", "b", "v"]
        );
    }

    #[test]
    fn create_fork_preserves_source_error_messages() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _fixture = BranchFixture::new();
        assert_eq!(create_fork(None).unwrap_err(), "No conversation to branch");
        fs::write(get_transcript_path(None), "").unwrap();
        assert_eq!(create_fork(None).unwrap_err(), "No conversation to branch");
        fs::write(
            get_transcript_path(None),
            "{\"type\":\"custom-title\",\"customTitle\":\"only\"}\n",
        )
        .unwrap();
        assert_eq!(create_fork(None).unwrap_err(), "No messages to branch");
        fs::write(get_transcript_path(None), "null\n").unwrap();
        assert_eq!(
            create_fork(None).unwrap_err(),
            "null is not an object (evaluating 'entry.type')"
        );
    }

    #[test]
    fn call_prepares_fork_saves_explicit_title_and_only_carries_source_log_metadata() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _write_enabled = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let fixture = BranchFixture::new();
        let original_id = get_session_id();
        let user = json!({"type":"user","uuid":"u","parentUuid":"old-parent","sessionId":original_id,
            "timestamp":"2026-09-12T00:00:00Z","slug":"original-plan","unknown":{"retained":true},
            "message":{"role":"user","content":"first\nprompt"}});
        fixture.write_current(&[
            user.clone(),
            json!({"type":"custom-title","sessionId":original_id,"customTitle":"do not inherit"}),
            json!({"type":"agent-name","sessionId":original_id,"agentName":"do not inherit"}),
            json!({"type":"worktree-state","sessionId":original_id,"worktreeSession":{"path":"do not inherit"}}),
            json!({"type":"content-replacement","sessionId":original_id,"replacements":[{"toolUseId":"t","preview":"small"}]}),
        ]);
        let prepared = call("\u{feff} chosen \u{feff}").unwrap();
        let target = prepared.target;
        assert_eq!(
            get_session_id(),
            original_id,
            "resume is awaited by the caller"
        );
        assert_eq!(
            target.entrypoint,
            Some(crate::types::command::ResumeEntrypoint::Fork)
        );
        let mut expected_user = user;
        expected_user["sessionId"] = json!(target.session_id);
        assert_eq!(target.entries, vec![expected_user]);
        let fork_path = get_transcript_path_for_session(&target.session_id);
        assert_eq!(
            target.metadata,
            crate::commands::resume::ResumeMetadata {
                session_id: Some(target.session_id.clone()),
                custom_title: Some("chosen (Branch)".to_string()),
                full_path: Some(fork_path.to_string_lossy().into_owned()),
                content_replacements: vec![json!({"toolUseId":"t","preview":"small"})],
                ..Default::default()
            }
        );
        assert_eq!(
            prepared.success_message,
            format!(
                "Branched conversation \"chosen\". You are now in the branch.\nTo resume the original: claude -r {original_id}"
            )
        );
        let disk = parse_jsonl(&fs::read(fork_path).unwrap());
        assert_eq!(
            disk.last().unwrap(),
            &json!({
                "type":"custom-title","sessionId":target.session_id,"customTitle":"chosen (Branch)"
            })
        );
        let default = call(" \t\u{feff}").unwrap();
        assert_eq!(
            default.target.metadata.custom_title.as_deref(),
            Some("first prompt (Branch)")
        );
        assert_eq!(
            default.success_message,
            format!(
                "Branched conversation. You are now in the branch.\nTo resume the original: claude -r {original_id}"
            )
        );
    }

    #[test]
    fn call_returns_creation_error_for_command_on_done_without_switching() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _fixture = BranchFixture::new();
        let original_id = get_session_id();
        assert_eq!(
            call("chosen").err().as_deref(),
            Some("No conversation to branch")
        );
        assert_eq!(get_session_id(), original_id);
    }

    #[test]
    fn get_unique_fork_name_matches_bun_collision_case_regex_and_number_gaps() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let fixture = BranchFixture::new();
        assert_eq!(get_unique_fork_name("new"), "new (Branch)");
        for title in [
            "A.*[X] (BRANCH)",
            "a.*[x] (Branch 2)",
            "a.*[x] (Branch 4)",
            "aZZx (Branch 3)",
            "topic (branch)",
            "TOPIC (Branch 2)",
            "Zero (Branch)",
            "Zero (Branch 0002)",
            "Zero (Branch 4)",
        ] {
            fixture.write_named_session(title);
        }
        assert_eq!(get_unique_fork_name("a.*[x]"), "a.*[x] (Branch 3)");
        // Source search is case-insensitive, but number extraction is not.
        assert_eq!(get_unique_fork_name("Topic"), "Topic (Branch 2)");
        assert_eq!(get_unique_fork_name("Zero"), "Zero (Branch 3)");
    }

    #[test]
    fn call_records_creation_event_matches_official_count_title_and_failure_boundary() {
        // CC branch.ts:254-257, executed in Bun with traceable dependencies:
        // create -> save title -> event -> resume -> done. Creation errors
        // emit no event; resume errors do not undo the prior creation event.
        // This native call prepares resume without switching the session.
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _write = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let fixture = BranchFixture::new();
        let original_id = get_session_id();
        let before = crate::services::analytics::queued_events_for_test().len();
        fixture.write_current(&[
            json!({"type":"user","uuid":"u","sessionId":original_id,
                "message":{"role":"user","content":"first prompt"}}),
            json!({"type":"assistant","uuid":"a","sessionId":original_id,
                "message":{"role":"assistant","content":[{"type":"text","text":"answer"}]}}),
            json!({"type":"system","uuid":"s","sessionId":original_id,"content":"system"}),
            json!({"type":"user","uuid":"side","sessionId":original_id,"isSidechain":true}),
            json!({"type":"progress","uuid":"progress","sessionId":original_id}),
            json!({"type":"custom-title","sessionId":original_id,"customTitle":"old"}),
        ]);
        for (args, has_custom_title) in [(" chosen ", true), (" \t\u{feff}", false)] {
            let prepared = call(args).unwrap();
            assert_eq!(get_session_id(), original_id, "event precedes resume");
            assert_eq!(prepared.target.entries.len(), 3);
            let rows = parse_jsonl(&fs::read(prepared.target.metadata.full_path.unwrap()).unwrap());
            assert_eq!(rows.last().unwrap()["type"], "custom-title");
            assert_eq!(
                crate::services::analytics::queued_events_for_test()
                    .last()
                    .unwrap(),
                &(
                    "tengu_conversation_forked".to_string(),
                    json!({
                        "message_count": 3, "has_custom_title": has_custom_title,
                    })
                )
            );
        }
        fixture.write_current(&[json!({"type":"custom-title","customTitle":"only metadata"})]);
        assert_eq!(
            call("chosen").err().as_deref(),
            Some("No messages to branch")
        );
        fs::remove_file(get_transcript_path(None)).unwrap();
        assert_eq!(
            call("chosen").err().as_deref(),
            Some("No conversation to branch")
        );
        let events = crate::services::analytics::queued_events_for_test();
        assert_eq!(
            serde_json::to_value(&events[before..]).unwrap(),
            json!([
                ["tengu_conversation_forked", {"message_count":3,"has_custom_title":true}],
                ["tengu_conversation_forked", {"message_count":3,"has_custom_title":false}],
            ])
        );
    }
}
