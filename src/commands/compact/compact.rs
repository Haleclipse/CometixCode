//! Model-backed `/compact` command owner.
//!
//! Maps to CC `commands/compact/compact.ts`. REPL owns only launch snapshots
//! and applying the returned typed result; compaction policy and cleanup live
//! here, as in the source command.

use crate::services::compact::auto_compact::AutoCompactCacheSafeParams;
use crate::services::compact::compact::CompactionResult;
use crate::types::message::Message;

#[derive(Clone, Debug)]
pub struct CompactCommandCacheContext {
    pub system_prompt: crate::services::api::request::SystemPrompt,
    pub user_context: std::collections::BTreeMap<String, String>,
    pub system_context: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
pub struct CompactCommandResult {
    pub compaction_result: CompactionResult,
    pub display_text: String,
}

/// Maps to CC `commands/compact/compact.ts:231-246` `buildDisplayText(...)`.
/// Upgrade-tip delivery remains with the future model-upgrade service; the
/// command preserves source ordering for the transcript shortcut and hook text.
pub fn build_display_text(
    verbose: bool,
    transcript_shortcut: &str,
    user_display_message: Option<&str>,
    upgrade_message: Option<&str>,
) -> String {
    let mut details = Vec::new();
    if !verbose {
        details.push(format!("({transcript_shortcut} to see full summary)"));
    }
    if let Some(message) = user_display_message.filter(|message| !message.is_empty()) {
        details.push(message.to_string());
    }
    if let Some(message) = upgrade_message.filter(|message| !message.is_empty()) {
        details.push(message.to_string());
    }
    format!("Compacted {}", details.join("\n"))
}

/// Maps to CC `commands/compact/compact.ts:40-127` `call(...)`, excluding the
/// still-explicit SessionMemory/reactive alternatives. This is the traditional
/// model-backed branch: boundary projection → replacement replay → compact →
/// cleanup.
pub async fn call<F>(
    messages: Vec<Message>,
    context: &crate::tool::ToolUseContext,
    cache_context: F,
    custom_instructions: &str,
) -> Result<CompactCommandResult, String>
where
    F: FnOnce() -> CompactCommandCacheContext + Send,
{
    let messages = crate::utils::messages::get_messages_after_compact_boundary(&messages);
    if messages.is_empty() {
        return Err("No messages to compact".to_string());
    }

    // @nyan: `/compact` projects existing replacements only. The compaction
    // request itself is the single owner allowed to add new mask decisions.
    let compact_messages = crate::services::compact::micro_compact::project_content_replacements(
        messages,
        context.content_replacement_state.as_ref(),
    );
    let cache_context = cache_context();
    let cache_safe_params = AutoCompactCacheSafeParams {
        system_prompt: cache_context.system_prompt,
        user_context: cache_context.user_context,
        system_context: cache_context.system_context,
        fork_context_messages: compact_messages.clone(),
    };
    let instructions = (!custom_instructions.is_empty()).then_some(custom_instructions);
    // @nyan: memory M2.1 — extract before the summary replaces history.
    // Failure or timeout is logged by the driver and never blocks `/compact`.
    crate::services::extract_memories::driver::extract_before_compact_for_summary(
        context,
        &cache_safe_params,
    )
    .await;
    // @nyan (compaction C7): manual compaction keeps the same recent tail as
    // autocompact; with no usable cut point it falls back to the full summary.
    let keep_pivot = crate::services::compact::micro_compact::content_mask_enabled()
        .then(|| {
            crate::services::compact::auto_compact::keep_recent_pivot(
                &compact_messages,
                crate::utils::settings::get_initial_settings()
                    .nyan
                    .map(|settings| settings.compaction().keep_recent_tokens())
                    .unwrap_or(20_000),
            )
        })
        .flatten();
    let compaction_result = if let Some(pivot_index) = keep_pivot {
        crate::services::compact::compact::partial_compact_conversation(
            compact_messages,
            pivot_index,
            context,
            &cache_safe_params,
            instructions,
            crate::types::message::PartialCompactDirection::UpTo,
        )
        .await?
    } else {
        crate::services::compact::compact::compact_conversation(
            compact_messages,
            context,
            &cache_safe_params,
            false,
            instructions,
            false,
        )
        .await?
    };

    crate::services::compact::post_compact_cleanup::run_post_compact_cleanup(None);
    let shortcut = crate::keybindings::shortcut_format::get_shortcut_display(
        "app:toggleTranscript",
        &crate::keybindings::types::ContextName::Global,
        "ctrl+o",
    );
    let mut display_text = build_display_text(
        context.verbose,
        &shortcut,
        compaction_result.user_display_message.as_deref(),
        None,
    );
    // @nyan (compaction C10): a summary model without the main cache pays full
    // input price, so surface whether this run actually read the cache.
    let cache_read = compaction_result
        .compaction_usage
        .as_ref()
        .map_or(0, |usage| usage.cache_read_input_tokens);
    display_text.push_str(&if cache_read > 0 {
        format!("\n(cache hit: {cache_read} tokens)")
    } else {
        "\n(cache miss: full input price)".to_string()
    });
    Ok(CompactCommandResult {
        compaction_result,
        display_text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn call_rejects_empty_history_before_building_cache_context() {
        let cache_built = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cache_built_for_call = cache_built.clone();
        let result = call(
            Vec::new(),
            &crate::tool::ToolUseContext::default(),
            move || {
                cache_built_for_call.store(true, std::sync::atomic::Ordering::SeqCst);
                CompactCommandCacheContext {
                    system_prompt: Vec::new(),
                    user_context: std::collections::BTreeMap::new(),
                    system_context: std::collections::BTreeMap::new(),
                }
            },
            "",
        )
        .await;
        assert_eq!(result.unwrap_err(), "No messages to compact");
        assert!(!cache_built.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn display_text_matches_official_detail_order() {
        assert_eq!(
            build_display_text(false, "ctrl+o", Some("Hook output"), Some("Upgrade tip")),
            "Compacted (ctrl+o to see full summary)\nHook output\nUpgrade tip"
        );
        assert_eq!(build_display_text(true, "ctrl+o", None, None), "Compacted ");
    }
}
