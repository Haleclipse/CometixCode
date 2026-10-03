//! Copy-on-write speculative execution for prompt suggestions.
//! Maps to CC `services/PromptSuggestion/speculation.ts`.

use crate::state::app_state_store::{
    ActiveSpeculationState, CompletionBoundary, PipelinedSuggestion, SpeculationState,
};
use crate::tool::{AbortController, CanUseToolCallback, ToolUseContext};
use crate::types::message::{AssistantContent, Message, UserContent, UserMessage};
use crate::types::permissions::{PermissionDecision, PermissionDecisionReason, PermissionMode};
use crate::utils::forked_agent::{CacheSafeParams, ForkedAgentParams, SubagentContextOverrides};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const MAX_SPECULATION_TURNS: u32 = 20;
const MAX_SPECULATION_MESSAGES: usize = 100;
const WRITE_TOOLS: &[&str] = &["Edit", "Write", "NotebookEdit"];
const SAFE_READ_ONLY_TOOLS: &[&str] = &[
    "Read",
    "Glob",
    "Grep",
    "ToolSearch",
    "LSP",
    "TaskGet",
    "TaskList",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpeculationResult {
    pub messages: Vec<Message>,
    pub boundary: Option<CompletionBoundary>,
    pub time_saved_ms: u64,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn overlay_path(id: &str) -> PathBuf {
    crate::utils::permissions::filesystem::get_claude_temp_dir()
        .join("speculation")
        .join(std::process::id().to_string())
        .join(id)
}

async fn safe_remove_overlay(path: &Path) {
    let _ = tokio::fs::remove_dir_all(path).await;
}

/// Maps to: CC `speculation.ts:84-97#denySpeculation(message, reason)` —
/// `{ behavior: 'deny', message, decisionReason: { type: 'other', reason } }`.
fn deny(message: impl Into<String>, reason: impl Into<String>) -> PermissionDecision {
    PermissionDecision::Deny {
        message: message.into(),
        decision_reason: PermissionDecisionReason::Other {
            reason: reason.into(),
        },
        tool_use_id: None,
    }
}

/// Maps to: CC `speculation.ts` allow literals (`:518-525`, `:553-560`,
/// `:564-571`, `:600-607`) — `{ behavior: 'allow', updatedInput: input,
/// decisionReason: { type: 'other', reason } }`.
fn allow(input: serde_json::Value, reason: impl Into<String>) -> PermissionDecision {
    PermissionDecision::Allow {
        updated_input: Some(input),
        user_modified: None,
        decision_reason: Some(PermissionDecisionReason::Other {
            reason: reason.into(),
        }),
        tool_use_id: None,
        accept_feedback: None,
        content_blocks: Vec::new(),
    }
}

fn update_active(
    context: &ToolUseContext,
    id: &str,
    update: impl FnOnce(&mut ActiveSpeculationState),
) {
    // Maps to: CC `updateActiveSpeculationState` (speculation.ts:310-327).
    // Two source branches, both projected verbatim:
    //   :315  `if (prev.speculation.status !== 'active') return prev`
    //   :319-322 `hasChanges` — the updater's fields are compared against the
    //            current ones and an all-equal update returns `prev`.
    //
    // The id check is Cometix-only and is retained here (unlike in
    // `reset_speculation_state`) because the source's updater closes over the
    // speculation it belongs to, whereas the Rust helper is reached by id from
    // detached tasks: writing an old task's boundary into a NEWER active
    // speculation would corrupt it, which the source cannot do. It only ever
    // converts a would-be cross-write into a `Same`, never suppresses a write
    // to the matching speculation.
    //
    // `hasChanges` is projected by [`speculation_has_changes`], which reproduces
    // the source's REFERENCE comparison. An earlier version of this used
    // `ActiveSpeculationState`'s structural `PartialEq`, which is the opposite
    // rule and suppressed writes CC installs — see that function's docs.
    context.set_app_state_decided(|prev| {
        let matches_active =
            matches!(&prev.speculation, SpeculationState::Active(active) if active.id == id);
        if !matches_active {
            return crate::state::store::UpdateDecision::Same(());
        }
        let mut next = (**prev).clone();
        let SpeculationState::Active(active) = &mut next.speculation else {
            return crate::state::store::UpdateDecision::Same(());
        };
        let before = active.clone();
        update(active);
        if !speculation_has_changes(&before, active) {
            // CC :322 `if (!hasChanges) return prev`.
            return crate::state::store::UpdateDecision::Same(());
        }
        crate::state::store::UpdateDecision::Replace {
            next: std::sync::Arc::new(next),
            result: (),
        }
    });
}

/// Maps to: CC `speculation.ts:319-321` `hasChanges`:
///
/// ```js
/// const hasChanges = Object.entries(updates).some(
///   ([key, value]) => current[key as keyof ActiveSpeculationState] !== value,
/// )
/// ```
///
/// `!==` is **reference** inequality. A scalar compares by value, but an
/// object-valued field the updater assigned is a freshly allocated object and
/// is therefore never equal to the previous one — so CC installs a new root
/// even when the contents match. Contract D's `JsObjectIs` fixes the same rule
/// ("fresh-object never-equal").
///
/// This previously used `ActiveSpeculationState`'s structural `PartialEq`,
/// which is the inverse: a repeated identical `pipelined_suggestion` (it
/// carries no timestamp, `speculation.rs` pipeline path) compared equal and was
/// suppressed as `Same`, where CC replaces the root and re-renders.
///
/// Boundary, deliberately wider than the source: CC compares only the keys the
/// updater RETURNED, while a Rust `FnOnce(&mut _)` cannot report which fields
/// it touched. Object-valued fields are therefore treated as changed whenever
/// either side is present. The only divergence this could cause is an extra
/// notification when an updater leaves an existing object untouched, and no
/// call site does that — the one updater that writes a scalar alone
/// (`tool_use_count`) is gated by its caller on `count > 0`. Erring toward
/// notifying also matches Contract A clause 1, which requires a value-equal
/// fresh root to notify.
fn speculation_has_changes(
    before: &ActiveSpeculationState,
    after: &ActiveSpeculationState,
) -> bool {
    // Scalars: value semantics, exactly what `!==` does on primitives.
    if before.id != after.id
        || before.start_time != after.start_time
        || before.suggestion_length != after.suggestion_length
        || before.tool_use_count != after.tool_use_count
        || before.is_pipelined != after.is_pipelined
    {
        return true;
    }
    // Object-valued: `!==` against a freshly built object is always true.
    // Only "absent before and after" is genuinely unchanged.
    before.boundary.is_some()
        || after.boundary.is_some()
        || before.pipelined_suggestion.is_some()
        || after.pipelined_suggestion.is_some()
}

/// Maps to: CC `resetSpeculationState` (speculation.ts:330-335).
///
/// The source's ONLY guard is `if (prev.speculation.status === 'idle') return
/// prev` — it resets whatever is active, including when a stale async
/// speculation finishes after a newer one has started. Cometix previously
/// added an id match here, which turned that exact case into a `Same` and left
/// the newer speculation running where the source would have cleared it. That
/// deviation had no L1/L2 authorization and "safer" is not a licence to change
/// observable behaviour, so it is removed (P5 re-review, 2026-08-03).
fn reset_speculation_state(context: &ToolUseContext) {
    context.set_app_state_decided(|prev| {
        if matches!(&prev.speculation, SpeculationState::Idle) {
            return crate::state::store::UpdateDecision::Same(());
        }
        let mut next = (**prev).clone();
        next.speculation = SpeculationState::Idle;
        crate::state::store::UpdateDecision::Replace {
            next: std::sync::Arc::new(next),
            result: (),
        }
    });
}

/// Maps to CC `isSpeculationEnabled()`; telemetry is intentionally omitted.
pub fn is_speculation_enabled() -> bool {
    crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::Speculation,
    ) && crate::utils::config::load_global_config()
        .speculation_enabled
        .unwrap_or(true)
}

fn path_key(input: &serde_json::Value) -> &'static str {
    if input.get("notebook_path").is_some() {
        "notebook_path"
    } else if input.get("path").is_some() {
        "path"
    } else {
        "file_path"
    }
}

fn relative_to_cwd(cwd: &Path, file_path: &str) -> Option<(PathBuf, String)> {
    let input_path = PathBuf::from(file_path);
    let absolute = if input_path.is_absolute() {
        input_path
    } else {
        cwd.join(input_path)
    };
    let relative = absolute.strip_prefix(cwd).ok()?.to_path_buf();
    if relative.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir | std::path::Component::RootDir
        )
    }) {
        return None;
    }
    let relative_string = relative.to_string_lossy().to_string();
    Some((absolute, relative_string))
}

fn successful_tool_result_count(message: &Message) -> usize {
    let Message::User(user) = message else {
        return 0;
    };
    user.content
        .iter()
        .filter(|block| matches!(block, UserContent::ToolResult(result) if !result.is_error))
        .count()
}

pub async fn start_speculation(
    suggestion_text: String,
    cache_safe_params: CacheSafeParams,
    is_pipelined: bool,
) -> anyhow::Result<()> {
    start_speculation_with_deps(
        suggestion_text,
        cache_safe_params,
        is_pipelined,
        crate::query::deps::production_deps(),
    )
    .await
}

pub(crate) async fn start_speculation_with_deps<D>(
    suggestion_text: String,
    cache_safe_params: CacheSafeParams,
    is_pipelined: bool,
    deps: D,
) -> anyhow::Result<()>
where
    D: crate::query::deps::QueryDeps,
{
    if !is_speculation_enabled() {
        return Ok(());
    }
    let root_context = cache_safe_params.tool_use_context.clone();
    abort_speculation(&root_context).await;

    let id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
    let abort_controller = AbortController::child_of(root_context.abort_controller.clone());
    if abort_controller.is_aborted() {
        return Ok(());
    }
    let start_time = now_ms();
    let overlay = overlay_path(&id);
    if tokio::fs::create_dir_all(&overlay).await.is_err() {
        return Ok(());
    }
    let messages = Arc::new(Mutex::new(Vec::<Message>::new()));
    let written_paths = Arc::new(Mutex::new(BTreeSet::<String>::new()));
    let cache_safe_params = Arc::new(cache_safe_params);
    root_context.set_app_state(|state| {
        state.speculation = SpeculationState::Active(ActiveSpeculationState {
            id: id.clone(),
            abort_controller: abort_controller.clone(),
            start_time,
            messages: messages.clone(),
            written_paths: written_paths.clone(),
            boundary: None,
            suggestion_length: suggestion_text.encode_utf16().count(),
            tool_use_count: 0,
            is_pipelined,
            cache_safe_params: cache_safe_params.clone(),
            pipelined_suggestion: None,
        });
    });

    let callback_context = root_context.clone();
    let callback_id = id.clone();
    let callback_abort = abort_controller.clone();
    let callback_overlay = overlay.clone();
    let callback_cwd = root_context.effective_cwd();
    let callback_written_paths = written_paths.clone();
    let can_use_tool = CanUseToolCallback::new_async(
        move |tool, input, _context, _assistant, _tool_use_id, _force| {
            let tool_name = tool.name.clone();
            let mut input = input.clone();
            let context = callback_context.clone();
            let id = callback_id.clone();
            let abort = callback_abort.clone();
            let overlay = callback_overlay.clone();
            let cwd = callback_cwd.clone();
            let written_paths = callback_written_paths.clone();
            Box::pin(async move {
                let is_write = WRITE_TOOLS.contains(&tool_name.as_str());
                let is_safe_read = SAFE_READ_ONLY_TOOLS.contains(&tool_name.as_str());
                if is_write {
                    let permission = context
                        .get_app_state()
                        .map(|state| state.tool_permission_context.clone());
                    let can_auto_accept = permission.as_ref().is_some_and(|permission| {
                        matches!(
                            permission.mode,
                            PermissionMode::AcceptEdits | PermissionMode::BypassPermissions
                        ) || (permission.mode == PermissionMode::Plan
                            && permission.is_bypass_permissions_mode_available)
                    });
                    if !can_auto_accept {
                        let file_path = input
                            .get("file_path")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        update_active(&context, &id, |active| {
                            active.boundary = Some(CompletionBoundary::Edit {
                                tool_name: tool_name.clone(),
                                file_path,
                                completed_at: now_ms(),
                            });
                        });
                        abort.abort();
                        // CC speculation.ts:490-493.
                        return deny(
                            "Speculation paused: file edit requires permission",
                            "speculation_edit_boundary",
                        );
                    }
                }

                if is_write || is_safe_read {
                    let key = path_key(&input);
                    if let Some(file_path) = input.get(key).and_then(serde_json::Value::as_str) {
                        let Some((absolute, relative)) = relative_to_cwd(&cwd, file_path) else {
                            if is_write {
                                // CC speculation.ts:513-516.
                                return deny(
                                    "Write outside cwd not allowed during speculation",
                                    "speculation_write_outside_root",
                                );
                            }
                            // CC speculation.ts:518-525.
                            return allow(input, "speculation_read_outside_root");
                        };
                        if is_write {
                            let first_write = {
                                let mut paths = written_paths
                                    .lock()
                                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                                paths.insert(relative.clone())
                            };
                            let overlay_file = overlay.join(&relative);
                            if first_write {
                                if let Some(parent) = overlay_file.parent() {
                                    let _ = tokio::fs::create_dir_all(parent).await;
                                }
                                let _ = tokio::fs::copy(&absolute, &overlay_file).await;
                            }
                            input[key] = serde_json::Value::String(
                                overlay_file.to_string_lossy().to_string(),
                            );
                        } else {
                            let was_written = written_paths
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner())
                                .contains(&relative);
                            if was_written {
                                input[key] = serde_json::Value::String(
                                    overlay.join(&relative).to_string_lossy().to_string(),
                                );
                            }
                        }
                        // CC speculation.ts:553-560.
                        return allow(input, "speculation_file_access");
                    }
                    if is_safe_read {
                        // CC speculation.ts:564-571.
                        return allow(input, "speculation_read_default_cwd");
                    }
                }

                if tool_name == crate::tools::bash_tool::tool_name::BASH_TOOL_NAME {
                    let command = input
                        .get("command")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    if command.is_empty()
                        || crate::tools::bash_tool::bash_permissions::command_has_any_cd(&command)
                        || !crate::tools::bash_tool::read_only_validation::check_read_only_constraints(
                            &command,
                            &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
                        )
                    {
                        update_active(&context, &id, |active| {
                            active.boundary = Some(CompletionBoundary::Bash {
                                command: command.clone(),
                                completed_at: now_ms(),
                            });
                        });
                        abort.abort();
                        // CC speculation.ts:594-597.
                        return deny(
                            "Speculation paused: bash boundary",
                            "speculation_bash_boundary",
                        );
                    }
                    // CC speculation.ts:600-607.
                    return allow(input, "speculation_readonly_bash");
                }

                let detail = ["url", "file_path", "path", "command"]
                    .into_iter()
                    .find_map(|key| {
                        input
                            .get(key)
                            .and_then(serde_json::Value::as_str)
                            .filter(|value| !value.is_empty())
                    })
                    .unwrap_or_default()
                    .to_string()
                    .chars()
                    .take(200)
                    .collect();
                update_active(&context, &id, |active| {
                    active.boundary = Some(CompletionBoundary::DeniedTool {
                        tool_name: tool_name.clone(),
                        detail,
                        completed_at: now_ms(),
                    });
                });
                abort.abort();
                // CC speculation.ts:628-631.
                deny(
                    format!("Tool {tool_name} not allowed during speculation"),
                    "speculation_unknown_tool",
                )
            })
        },
    );

    let on_message_context = root_context.clone();
    let on_message_id = id.clone();
    let on_message_abort = abort_controller.clone();
    let on_message_messages = messages.clone();
    let on_message = Arc::new(move |message: &Message| {
        if !matches!(message, Message::Assistant(_) | Message::User(_)) {
            return;
        }
        let count = successful_tool_result_count(message);
        let len = {
            let mut output = on_message_messages
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            output.push(message.clone());
            output.len()
        };
        if count > 0 {
            update_active(&on_message_context, &on_message_id, |active| {
                active.tool_use_count = active.tool_use_count.saturating_add(count);
            });
        }
        if len >= MAX_SPECULATION_MESSAGES {
            on_message_abort.abort();
        }
    });

    let result = crate::utils::forked_agent::run_forked_agent_with_deps(
        ForkedAgentParams {
            prompt_messages: vec![Message::User(UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![UserContent::Text(suggestion_text.clone())],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            })],
            cache_safe_params: Arc::unwrap_or_clone(cache_safe_params.clone()),
            can_use_tool,
            query_source: crate::constants::query_source::QuerySource::Speculation,
            fork_label: "speculation".to_string(),
            overrides: Some(SubagentContextOverrides {
                abort_controller: Some(abort_controller.clone()),
                require_can_use_tool: Some(true),
                ..Default::default()
            }),
            max_output_tokens: None,
            max_turns: Some(MAX_SPECULATION_TURNS),
            on_message: Some(on_message),
            on_progress: None,
            skip_transcript: true,
            skip_cache_write: false,
        },
        deps,
    )
    .await;

    let result = match result {
        Ok(result) => result,
        Err(error) => {
            if !abort_controller.is_aborted() {
                abort_controller.abort();
                safe_remove_overlay(&overlay).await;
                reset_speculation_state(&root_context);
                return Err(error);
            }
            return Ok(());
        }
    };
    if abort_controller.is_aborted() {
        return Ok(());
    }
    if let Some(error) = result.api_errors.first() {
        abort_controller.abort();
        safe_remove_overlay(&overlay).await;
        reset_speculation_state(&root_context);
        return Err(anyhow::anyhow!(error.content.clone()));
    }
    let output_tokens = result.total_usage.output_tokens;
    update_active(&root_context, &id, |active| {
        active.boundary = Some(CompletionBoundary::Complete {
            completed_at: now_ms(),
            output_tokens,
        });
    });

    let pipeline_context = root_context.clone();
    let pipeline_id = id.clone();
    let pipeline_abort = abort_controller.clone();
    let pipeline_messages = messages.clone();
    let pipeline_cache = cache_safe_params.clone();
    tokio::spawn(async move {
        if pipeline_context.get_app_state().is_some_and(|state| {
            crate::services::prompt_suggestion::prompt_suggestion::get_suggestion_suppress_reason(
                &state,
            )
            .is_some()
        }) {
            return;
        }
        let speculated = pipeline_messages
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let mut augmented = pipeline_cache.fork_context_messages.as_ref().clone();
        augmented.push(Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![UserContent::Text(suggestion_text)],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        }));
        augmented.extend(speculated);
        let mut cache = Arc::unwrap_or_clone(pipeline_cache);
        cache.fork_context_messages = Arc::new(augmented);
        let child = AbortController::child_of(pipeline_abort);
        let prompt_id = crate::services::prompt_suggestion::prompt_suggestion::get_prompt_variant();
        let generated = crate::services::prompt_suggestion::prompt_suggestion::generate_suggestion(
            child.clone(),
            prompt_id,
            cache,
        )
        .await;
        let Ok(generated) = generated else { return };
        let Some(text) = generated.suggestion else {
            return;
        };
        if child.is_aborted()
            || crate::services::prompt_suggestion::prompt_suggestion::should_filter_suggestion(
                Some(&text),
                prompt_id,
            )
        {
            return;
        }
        update_active(&pipeline_context, &pipeline_id, |active| {
            active.pipelined_suggestion = Some(PipelinedSuggestion {
                text,
                prompt_id: prompt_id.official_name().to_string(),
                generation_request_id: generated.generation_request_id,
            });
        });
    });

    Ok(())
}

async fn copy_overlay_to_main(
    overlay: &Path,
    written_paths: &Arc<Mutex<BTreeSet<String>>>,
    cwd: &Path,
) -> bool {
    let paths = written_paths
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let mut copied = true;
    for relative in paths {
        let source = overlay.join(&relative);
        let destination = cwd.join(&relative);
        if let Some(parent) = destination.parent() {
            if tokio::fs::create_dir_all(parent).await.is_err() {
                copied = false;
                continue;
            }
        }
        if tokio::fs::copy(source, destination).await.is_err() {
            copied = false;
        }
    }
    copied
}

pub async fn accept_speculation(
    context: &ToolUseContext,
    clean_message_count: usize,
) -> Option<SpeculationResult> {
    let state = context.get_app_state()?.speculation.clone();
    let SpeculationState::Active(active) = state else {
        return None;
    };
    Some(accept_active_speculation(context, &active, clean_message_count).await)
}

/// Maps to CC `acceptSpeculation(state, setAppState, cleanMessageCount)`: the
/// accepted render snapshot remains authoritative even if another state update
/// lands before overlay copying completes.
pub async fn accept_active_speculation(
    context: &ToolUseContext,
    active: &ActiveSpeculationState,
    clean_message_count: usize,
) -> SpeculationResult {
    let accepted_at = now_ms();
    active.abort_controller.abort();
    let overlay = overlay_path(&active.id);
    if clean_message_count > 0 {
        let _ =
            copy_overlay_to_main(&overlay, &active.written_paths, &context.effective_cwd()).await;
    }
    safe_remove_overlay(&overlay).await;
    let messages = active
        .messages
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let boundary = context
        .get_app_state()
        .and_then(|state| match &state.speculation {
            SpeculationState::Active(latest) if latest.id == active.id => latest.boundary.clone(),
            _ => active.boundary.clone(),
        });
    let completed_at = boundary
        .as_ref()
        .map(boundary_completed_at)
        .unwrap_or(accepted_at);
    let time_saved_ms = accepted_at
        .min(completed_at)
        .saturating_sub(active.start_time);
    context.set_app_state(|state| {
        state.speculation = SpeculationState::Idle;
        state.speculation_session_time_saved_ms = state
            .speculation_session_time_saved_ms
            .saturating_add(time_saved_ms);
    });
    if time_saved_ms > 0 {
        let _ = crate::utils::session_storage::record_speculation_accept(time_saved_ms);
    }
    SpeculationResult {
        messages,
        boundary,
        time_saved_ms,
    }
}

pub(crate) fn create_speculation_feedback_message(
    messages: &[Message],
    boundary: Option<&CompletionBoundary>,
    time_saved_ms: u64,
    session_total_ms: u64,
) -> Option<Message> {
    if !crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::TelemetryPayloads,
    ) || messages.is_empty()
        || time_saved_ms == 0
    {
        return None;
    }
    let tool_uses = messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => Some(&user.content),
            _ => None,
        })
        .flatten()
        .filter(|content| matches!(content, UserContent::ToolResult(result) if !result.is_error))
        .count();
    let first = if tool_uses > 0 {
        format!(
            "Speculated {tool_uses} tool {}",
            if tool_uses == 1 { "use" } else { "uses" }
        )
    } else {
        let turns = messages.len();
        format!(
            "Speculated {turns} {}",
            if turns == 1 { "turn" } else { "turns" }
        )
    };
    let mut parts = vec![first];
    if let Some(CompletionBoundary::Complete { output_tokens, .. }) = boundary {
        parts.push(format!(
            "{} tokens",
            crate::utils::format::format_number(*output_tokens)
        ));
    }
    let session_suffix = if session_total_ms != time_saved_ms {
        format!(
            " ({} this session)",
            crate::utils::format::format_duration(session_total_ms)
        )
    } else {
        String::new()
    };
    parts.push(format!(
        "+{} saved{session_suffix}",
        crate::utils::format::format_duration(time_saved_ms)
    ));
    Some(Message::System(
        crate::types::message::SystemMessage::informational(
            format!("[ANT-ONLY] {}", parts.join(" · ")),
            crate::types::message::SystemMessageLevel::Warning,
        ),
    ))
}

fn boundary_completed_at(boundary: &CompletionBoundary) -> u64 {
    match boundary {
        CompletionBoundary::Complete { completed_at, .. }
        | CompletionBoundary::Bash { completed_at, .. }
        | CompletionBoundary::Edit { completed_at, .. }
        | CompletionBoundary::DeniedTool { completed_at, .. } => *completed_at,
    }
}

fn take_active_speculation_from_store(
    store: &crate::state::store::AppStore,
) -> Option<ActiveSpeculationState> {
    // P3 §3a: CC returns `prev` untouched when speculation is not Active;
    // Same carries the None result without an effect pass.
    store.set_state(|prev| {
        let SpeculationState::Active(active) = &prev.speculation else {
            return crate::state::store::UpdateDecision::Same(None);
        };
        let active = active.clone();
        let mut next = (**prev).clone();
        next.speculation = SpeculationState::Idle;
        crate::state::store::UpdateDecision::Replace {
            next: std::sync::Arc::new(next),
            result: Some(active),
        }
    })
}

fn schedule_overlay_cleanup(path: PathBuf) {
    // Maps to CC `speculation.ts:72-78` `safeRemoveOverlay`: a callback-style
    // `rm(..., () => {})` — fire-and-forget on the PROCESS (Node's fs pool),
    // guaranteed to run as long as the process lives. A3 audit (PORTING.md §
    // "Node-async → tokio"): the old `try_current()` put the removal on the ambient
    // runtime; scheduled from inside a turn, the cleanup died with it and the
    // overlay directory leaked.
    if let Some(handle) = crate::utils::process_runtime::runtime_handle_for_detached_work() {
        handle.spawn(async move {
            safe_remove_overlay(&path).await;
        });
    } else {
        std::thread::spawn(move || {
            let _ = std::fs::remove_dir_all(path);
        });
    }
}

/// Maps to CC `abortSpeculation(setAppState)` for non-REPL state mutations.
/// The suggestion text is intentionally untouched; only the precomputed active
/// response is invalidated. Returns false for idle/duplicate notifications.
pub fn abort_speculation_for_store(store: &crate::state::store::AppStore) -> bool {
    let Some(active) = take_active_speculation_from_store(store) else {
        return false;
    };
    active.abort_controller.abort();
    schedule_overlay_cleanup(overlay_path(&active.id));
    true
}

pub async fn abort_speculation(context: &ToolUseContext) {
    if let Some(store) = context
        .app_store
        .store
        .as_ref()
        .or(context.app_store.tasks_store.as_ref())
    {
        if let Some(active) = take_active_speculation_from_store(store) {
            active.abort_controller.abort();
            safe_remove_overlay(&overlay_path(&active.id)).await;
        }
        return;
    }

    let active = context
        .get_app_state()
        .and_then(|state| match &state.speculation {
            SpeculationState::Active(active) => Some(active.clone()),
            SpeculationState::Idle => None,
        });
    let Some(active) = active else {
        return;
    };
    active.abort_controller.abort();
    safe_remove_overlay(&overlay_path(&active.id)).await;
    reset_speculation_state(context);
}

/// Maps to CC `prepareMessagesForInjection(...)`.
pub fn prepare_messages_for_injection(messages: &[Message]) -> Vec<Message> {
    let successful_ids = messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => Some(&user.content),
            _ => None,
        })
        .flatten()
        .filter_map(|block| match block {
            UserContent::ToolResult(result)
                if !result.is_error
                    && !result
                        .content
                        .contains(crate::utils::messages::INTERRUPT_MESSAGE_FOR_TOOL_USE) =>
            {
                Some(result.tool_use_id.0.clone())
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();

    messages
        .iter()
        .filter_map(|message| match message {
            Message::Assistant(assistant) => {
                let mut assistant = assistant.clone();
                assistant.content.retain(|block| match block {
                    AssistantContent::Thinking { .. }
                    | AssistantContent::RedactedThinking { .. } => false,
                    AssistantContent::ToolUse(tool) => successful_ids.contains(&tool.id.0),
                    _ => true,
                });
                assistant
                    .has_model_content()
                    .then_some(Message::Assistant(assistant))
            }
            Message::User(user) => {
                let mut user = user.clone();
                user.content.retain(|block| match block {
                    UserContent::ToolResult(result) => {
                        successful_ids.contains(&result.tool_use_id.0)
                    }
                    UserContent::Text(text) | UserContent::MetaText(text) => {
                        text != crate::utils::messages::INTERRUPT_MESSAGE
                            && text != crate::utils::messages::INTERRUPT_MESSAGE_FOR_TOOL_USE
                            && !text.trim().is_empty()
                    }
                    _ => true,
                });
                (!user.content.is_empty()).then_some(Message::User(user))
            }
            message => Some(message.clone()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ids::ToolUseId;
    use crate::types::message::{AssistantMessage, StopReason, ToolResult, ToolUseBlock};
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    fn active_state(id: &str) -> ActiveSpeculationState {
        ActiveSpeculationState {
            id: id.to_string(),
            abort_controller: AbortController::default(),
            start_time: 1_000,
            messages: Arc::new(Mutex::new(Vec::new())),
            written_paths: Arc::new(Mutex::new(BTreeSet::new())),
            boundary: None,
            suggestion_length: 3,
            tool_use_count: 0,
            is_pipelined: false,
            cache_safe_params: Arc::new(crate::utils::forked_agent::CacheSafeParams {
                system_prompt: Default::default(),
                user_context: Default::default(),
                system_context: Default::default(),
                tool_use_context: ToolUseContext::default(),
                fork_context_messages: Arc::new(Vec::new()),
            }),
            pipelined_suggestion: None,
        }
    }

    /// Maps to: CC `speculation.ts:319-321`. `!==` is REFERENCE inequality, so
    /// re-assigning an object-valued field with identical contents still counts
    /// as a change and CC installs a new root.
    ///
    /// This is the case a structural `PartialEq` gets backwards. It is reachable
    /// through the pipelined-suggestion path, whose payload carries no timestamp
    /// to make successive writes differ.
    #[test]
    fn has_changes_treats_a_reassigned_object_as_changed_like_official() {
        let suggestion = || PipelinedSuggestion {
            text: "same text".to_string(),
            prompt_id: "same-prompt".to_string(),
            generation_request_id: None,
        };

        let mut before = active_state("spec-1");
        before.pipelined_suggestion = Some(suggestion());
        let mut after = before.clone();
        after.pipelined_suggestion = Some(suggestion());

        // Structurally identical — a value comparison would call this "no
        // change" and suppress the write CC performs.
        assert_eq!(before.pipelined_suggestion, after.pipelined_suggestion);
        assert!(
            speculation_has_changes(&before, &after),
            "a re-assigned object must count as changed (CC `!==`)"
        );
    }

    /// The scalar half of `!==`: primitives compare by value, so an unchanged
    /// count is genuinely unchanged.
    #[test]
    fn has_changes_compares_scalars_by_value_like_official() {
        let before = active_state("spec-1");
        let unchanged = before.clone();
        assert!(!speculation_has_changes(&before, &unchanged));

        let mut bumped = before.clone();
        bumped.tool_use_count += 1;
        assert!(speculation_has_changes(&before, &bumped));
    }

    #[test]
    fn injection_strips_thinking_interrupts_and_unresolved_tool_pairs() {
        let messages = vec![
            Message::Assistant(AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![
                    AssistantContent::Thinking {
                        text: "draft".to_string(),
                        signature: String::new(),
                    },
                    AssistantContent::ToolUse(ToolUseBlock {
                        id: ToolUseId("ok".to_string()),
                        name: "Read".to_string(),
                        input: serde_json::json!({}),
                    }),
                    AssistantContent::ToolUse(ToolUseBlock {
                        id: ToolUseId("pending".to_string()),
                        name: "Read".to_string(),
                        input: serde_json::json!({}),
                    }),
                ],
                model: None,
                stop_reason: Some(StopReason::ToolUse),
                usage: None,
            }),
            Message::User(UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![
                    UserContent::ToolResult(ToolResult {
                        tool_use_id: ToolUseId("ok".to_string()),
                        content: "done".to_string(),
                        is_error: false,
                        content_blocks: Vec::new(),
                        tool_use_result: None,
                    }),
                    UserContent::Text(crate::utils::messages::INTERRUPT_MESSAGE.to_string()),
                ],
                is_compact_summary: false,
                plan_content: None,
                image_paste_ids: None,
                is_visible_in_transcript_only: false,
                mcp_meta: None,
                source_tool_assistant_uuid: None,
                permission_mode: None,
                origin: None,
                summarize_metadata: None,
            }),
        ];

        let cleaned = prepare_messages_for_injection(&messages);

        assert!(matches!(
            &cleaned[0],
            Message::Assistant(assistant)
                if assistant.content.len() == 1
                    && matches!(&assistant.content[0], AssistantContent::ToolUse(tool) if tool.id.0 == "ok")
        ));
        assert!(matches!(
            &cleaned[1],
            Message::User(user) if user.content.len() == 1
        ));
    }

    #[test]
    fn ant_feedback_reports_tools_tokens_and_session_time() {
        let messages = vec![Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            content: vec![UserContent::ToolResult(ToolResult {
                tool_use_id: ToolUseId("toolu_ok".to_string()),
                content: "ok".to_string(),
                is_error: false,
                content_blocks: Vec::new(),
                tool_use_result: None,
            })],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })];
        let feedback = create_speculation_feedback_message(
            &messages,
            Some(&CompletionBoundary::Complete {
                completed_at: 10,
                output_tokens: 1234,
            }),
            2_000,
            5_000,
        );
        if crate::utils::build_profile::has_internal_capability(
            crate::utils::build_profile::InternalCapability::TelemetryPayloads,
        ) {
            let Message::System(feedback) = feedback.expect("internal feedback") else {
                panic!("feedback must be a system message");
            };
            let content = feedback.content().expect("informational content");
            assert!(content.contains("Speculated 1 tool use"));
            assert!(content.contains("1.2k tokens"));
            assert!(content.contains("this session"));
        } else {
            assert!(feedback.is_none());
        }
    }

    #[derive(Clone, Debug)]
    struct SingleToolBoundaryDeps {
        name: &'static str,
        input: serde_json::Value,
    }

    impl crate::query::deps::QueryDeps for SingleToolBoundaryDeps {
        fn call_model(
            &self,
            _request: crate::query::deps::CallModelRequest,
        ) -> crate::query::deps::CallModelStreamFuture {
            let name = self.name.to_string();
            let input = self.input.clone();
            Box::pin(async move {
                let (tx, rx) = tokio::sync::mpsc::channel(2);
                tx.send(
                    crate::services::api::claude::QueryModelStreamItem::Assistant(
                        AssistantMessage {
                            uuid: uuid::Uuid::new_v4().to_string(),
                            timestamp: chrono::Utc::now(),
                            content: vec![AssistantContent::ToolUse(ToolUseBlock {
                                id: ToolUseId("toolu_spec_boundary".to_string()),
                                name,
                                input,
                            })],
                            model: None,
                            stop_reason: Some(StopReason::ToolUse),
                            usage: None,
                        },
                    ),
                )
                .await
                .ok();
                Ok(rx)
            })
        }
    }

    #[derive(Clone, Debug)]
    struct CowThenBoundaryDeps {
        file_path: String,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl crate::query::deps::QueryDeps for CowThenBoundaryDeps {
        fn call_model(
            &self,
            _request: crate::query::deps::CallModelRequest,
        ) -> crate::query::deps::CallModelStreamFuture {
            let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let file_path = self.file_path.clone();
            Box::pin(async move {
                let (name, input) = if call == 0 {
                    (
                        "Write",
                        serde_json::json!({"file_path": file_path, "content": "overlay only"}),
                    )
                } else {
                    (
                        "WebFetch",
                        serde_json::json!({
                            "url": "https://example.com",
                            "prompt": "summarize"
                        }),
                    )
                };
                let (tx, rx) = tokio::sync::mpsc::channel(2);
                tx.send(
                    crate::services::api::claude::QueryModelStreamItem::Assistant(
                        AssistantMessage {
                            uuid: uuid::Uuid::new_v4().to_string(),
                            timestamp: chrono::Utc::now(),
                            content: vec![AssistantContent::ToolUse(ToolUseBlock {
                                id: ToolUseId(format!("toolu_cow_{call}")),
                                name: name.to_string(),
                                input,
                            })],
                            model: None,
                            stop_reason: Some(StopReason::ToolUse),
                            usage: None,
                        },
                    ),
                )
                .await
                .ok();
                Ok(rx)
            })
        }
    }

    async fn run_single_tool_boundary(
        name: &'static str,
        input: serde_json::Value,
        mode: PermissionMode,
    ) -> (
        CompletionBoundary,
        ToolUseContext,
        crate::state::store::AppStore,
    ) {
        let mut state = crate::state::app_state_store::AppState::default();
        let mut permission = (*state.tool_permission_context).clone();
        permission.mode = mode;
        state.tool_permission_context = Arc::new(permission.clone());
        let store = crate::state::store::AppStore::new(state, None);
        let context = ToolUseContext::with_permission_context(permission)
            .with_app_store(store.clone())
            .with_tools(crate::tools::get_all_base_tools())
            .with_main_loop_model("claude-test")
            .with_cwd_override(Some(PathBuf::from("/repo")));
        let cache = CacheSafeParams {
            system_prompt: vec!["system".to_string()],
            user_context: Default::default(),
            system_context: Default::default(),
            tool_use_context: context.clone(),
            fork_context_messages: Arc::new(Vec::new()),
        };
        start_speculation_with_deps(
            "continue".to_string(),
            cache,
            false,
            SingleToolBoundaryDeps { name, input },
        )
        .await
        .unwrap();
        let boundary = match &store.get().speculation {
            SpeculationState::Active(active) => active.boundary.clone().expect("boundary"),
            SpeculationState::Idle => panic!("speculation should remain at boundary"),
        };
        (boundary, context, store)
    }

    #[cfg(feature = "anthropic_internal")]
    #[tokio::test]
    async fn speculation_stops_at_edit_boundary_before_default_permission_prompt() {
        let mut state = crate::state::app_state_store::AppState::default();
        let mut permission = (*state.tool_permission_context).clone();
        permission.mode = PermissionMode::Default;
        state.tool_permission_context = Arc::new(permission.clone());
        let store = crate::state::store::AppStore::new(state, None);
        let mut context = ToolUseContext::with_permission_context(permission)
            .with_app_store(store.clone())
            .with_tools(crate::tools::get_all_base_tools())
            .with_main_loop_model("claude-test");
        context = context.with_cwd_override(Some(PathBuf::from("/repo")));
        let cache = CacheSafeParams {
            system_prompt: vec!["system".to_string()],
            user_context: Default::default(),
            system_context: Default::default(),
            tool_use_context: context.clone(),
            fork_context_messages: Arc::new(Vec::new()),
        };

        let result = start_speculation_with_deps(
            "make the change".to_string(),
            cache,
            false,
            SingleToolBoundaryDeps {
                name: "Write",
                input: serde_json::json!({
                    "file_path": "/repo/new.txt",
                    "content": "hello"
                }),
            },
        )
        .await;

        assert!(result.is_ok());
        let active = match &store.get().speculation {
            SpeculationState::Active(active) => active.clone(),
            SpeculationState::Idle => panic!("speculation should remain at boundary"),
        };
        assert!(matches!(
            active.boundary,
            Some(CompletionBoundary::Edit { ref tool_name, .. }) if tool_name == "Write"
        ));
        assert!(active.abort_controller.is_aborted());
        abort_speculation(&context).await;
        assert!(matches!(store.get().speculation, SpeculationState::Idle));
    }

    #[cfg(feature = "anthropic_internal")]
    #[tokio::test]
    async fn speculation_stops_at_bash_and_unknown_tool_boundaries() {
        // Match main/print's process runtime before the forked query loads plugins.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let (bash, bash_context, _) = run_single_tool_boundary(
            "Bash",
            serde_json::json!({"command": "echo changed > file.txt"}),
            PermissionMode::BypassPermissions,
        )
        .await;
        assert!(matches!(
            bash,
            CompletionBoundary::Bash { ref command, .. } if command.contains("changed")
        ));
        abort_speculation(&bash_context).await;

        let (denied, denied_context, _) = run_single_tool_boundary(
            "WebFetch",
            serde_json::json!({
                "url": "https://example.com",
                "prompt": "summarize"
            }),
            PermissionMode::BypassPermissions,
        )
        .await;
        assert!(matches!(
            denied,
            CompletionBoundary::DeniedTool { ref tool_name, ref detail, .. }
                if tool_name == "WebFetch" && detail == "https://example.com"
        ));
        abort_speculation(&denied_context).await;
    }

    #[cfg(feature = "anthropic_internal")]
    #[tokio::test]
    async fn accept_edits_mode_writes_only_to_overlay_until_acceptance() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        // Match main/print's process runtime before the forked query loads plugins.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let _write = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let root = std::env::temp_dir().join(format!("cometix-cow-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let destination = root.join("new.txt");
        let mut state = crate::state::app_state_store::AppState::default();
        let mut permission = (*state.tool_permission_context).clone();
        permission.mode = PermissionMode::AcceptEdits;
        state.tool_permission_context = Arc::new(permission.clone());
        let store = crate::state::store::AppStore::new(state, None);
        let context = ToolUseContext::with_permission_context(permission)
            .with_app_store(store.clone())
            .with_tools(crate::tools::get_all_base_tools())
            .with_main_loop_model("claude-test")
            .with_cwd_override(Some(root.clone()));
        let cache = CacheSafeParams {
            system_prompt: vec!["system".to_string()],
            user_context: Default::default(),
            system_context: Default::default(),
            tool_use_context: context.clone(),
            fork_context_messages: Arc::new(Vec::new()),
        };
        start_speculation_with_deps(
            "write then browse".to_string(),
            cache,
            false,
            CowThenBoundaryDeps {
                file_path: destination.to_string_lossy().to_string(),
                calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            },
        )
        .await
        .unwrap();

        let active = match &store.get().speculation {
            SpeculationState::Active(active) => active.clone(),
            SpeculationState::Idle => panic!("denied boundary should keep speculation active"),
        };
        assert!(
            matches!(
                active.boundary,
                Some(CompletionBoundary::DeniedTool { ref tool_name, .. }) if tool_name == "WebFetch"
            ),
            "unexpected boundary: {:?}; messages: {:?}",
            active.boundary,
            active
                .messages
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        );
        assert!(!destination.exists(), "main cwd must remain untouched");
        assert_eq!(
            std::fs::read_to_string(overlay_path(&active.id).join("new.txt")).unwrap(),
            "overlay only"
        );
        abort_speculation(&context).await;
        assert!(!overlay_path(&active.id).exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn accepting_speculation_copies_overlay_and_resets_state() {
        let _env_lock = TEST_ENV_LOCK.lock().unwrap();
        let _write = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let root = std::env::temp_dir().join(format!("cometix-spec-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let context = ToolUseContext::default()
            .with_app_store(store.clone())
            .with_cwd_override(Some(root.clone()));
        let cache = Arc::new(CacheSafeParams {
            system_prompt: Vec::new(),
            user_context: Default::default(),
            system_context: Default::default(),
            tool_use_context: context.clone(),
            fork_context_messages: Arc::new(Vec::new()),
        });
        let id = uuid::Uuid::new_v4().simple().to_string();
        let overlay = overlay_path(&id);
        std::fs::create_dir_all(overlay.join("src")).unwrap();
        std::fs::write(overlay.join("src/file.txt"), "speculated").unwrap();
        let active = ActiveSpeculationState {
            id,
            abort_controller: AbortController::default(),
            start_time: now_ms().saturating_sub(50),
            messages: Arc::new(Mutex::new(vec![Message::Assistant(AssistantMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![AssistantContent::Text("done".to_string())],
                model: None,
                stop_reason: Some(StopReason::EndTurn),
                usage: None,
            })])),
            written_paths: Arc::new(Mutex::new(BTreeSet::from(["src/file.txt".to_string()]))),
            boundary: Some(CompletionBoundary::Complete {
                completed_at: now_ms(),
                output_tokens: 4,
            }),
            suggestion_length: 4,
            tool_use_count: 1,
            is_pipelined: false,
            cache_safe_params: cache,
            pipelined_suggestion: None,
        };
        store.replace_with(|state| state.speculation = SpeculationState::Active(active));

        let result = accept_speculation(&context, 1)
            .await
            .expect("active speculation");

        assert!(matches!(
            result.boundary,
            Some(CompletionBoundary::Complete { .. })
        ));
        assert_eq!(
            std::fs::read_to_string(root.join("src/file.txt")).unwrap(),
            "speculated"
        );
        assert!(matches!(store.get().speculation, SpeculationState::Idle));
        assert!(!overlay.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn relative_to_cwd_rejects_outside_paths() {
        let cwd = Path::new("/repo");
        assert!(relative_to_cwd(cwd, "/repo/src/lib.rs").is_some());
        assert!(relative_to_cwd(cwd, "/other/file").is_none());
    }
}
