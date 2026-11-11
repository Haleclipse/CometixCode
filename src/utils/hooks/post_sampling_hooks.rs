//! Programmatic post-sampling hook registry.
//! Maps to CC `utils/hooks/postSamplingHooks.ts`.
//!
//! These hooks are internal (not settings.json shell hooks). CC runs them after
//! model sampling completes and before stop hooks / tool execution. Nyan keeps
//! the same boundary so future services can register side-effecting observers
//! without adding those concerns to `query.rs`.

use crate::constants::query_source::QuerySource;
use crate::services::api::request::SystemPrompt;
use crate::tool::ToolUseContext;
use crate::types::message::Message;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
#[cfg(not(test))]
use std::sync::{LazyLock, Mutex};

/// Maps to CC `REPLHookContext` in `utils/hooks/postSamplingHooks.ts`.
#[derive(Clone, Debug)]
pub struct REPLHookContext {
    pub messages: Vec<Message>,
    pub system_prompt: SystemPrompt,
    pub user_context: std::collections::BTreeMap<String, String>,
    pub system_context: std::collections::BTreeMap<String, String>,
    pub tool_use_context: ToolUseContext,
    pub query_source: Option<QuerySource>,
}

pub type PostSamplingHookFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
pub type PostSamplingHook = Arc<dyn Fn(REPLHookContext) -> PostSamplingHookFuture + Send + Sync>;

#[cfg(not(test))]
static POST_SAMPLING_HOOKS: LazyLock<Mutex<Vec<PostSamplingHook>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

#[cfg(test)]
thread_local! {
    static POST_SAMPLING_HOOKS: std::cell::RefCell<Vec<PostSamplingHook>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Maps to CC `registerPostSamplingHook(...)`.
#[cfg(not(test))]
pub fn register_post_sampling_hook(hook: PostSamplingHook) {
    POST_SAMPLING_HOOKS.lock().unwrap().push(hook);
}

/// Test-local storage variant of the production post-sampling hook registrar.
#[cfg(test)]
pub fn register_post_sampling_hook(hook: PostSamplingHook) {
    POST_SAMPLING_HOOKS.with(|hooks| hooks.borrow_mut().push(hook));
}

/// Maps to CC `clearPostSamplingHooks(...)`.
#[cfg(not(test))]
pub fn clear_post_sampling_hooks() {
    POST_SAMPLING_HOOKS.lock().unwrap().clear();
}

/// Test-local storage variant of the production post-sampling hook clearer.
#[cfg(test)]
pub fn clear_post_sampling_hooks() {
    POST_SAMPLING_HOOKS.with(|hooks| hooks.borrow_mut().clear());
}

/// Maps to CC `executePostSamplingHooks(...)`.
pub async fn execute_post_sampling_hooks(
    messages: Vec<Message>,
    system_prompt: SystemPrompt,
    user_context: std::collections::BTreeMap<String, String>,
    system_context: std::collections::BTreeMap<String, String>,
    tool_use_context: ToolUseContext,
    query_source: Option<QuerySource>,
) {
    #[cfg(not(test))]
    let hooks = POST_SAMPLING_HOOKS.lock().unwrap().clone();
    #[cfg(test)]
    let hooks = POST_SAMPLING_HOOKS.with(|hooks| hooks.borrow().clone());
    if hooks.is_empty() {
        return;
    }

    let context = REPLHookContext {
        messages,
        system_prompt,
        user_context,
        system_context,
        tool_use_context,
        query_source,
    };

    for hook in hooks {
        // CC logs and continues on post-sampling hook failures. Rust hook
        // futures do not carry errors, so panics are isolated by task/test
        // boundaries rather than propagated through the query loop.
        hook(context.clone()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn post_sampling_hooks_receive_official_context_shape() {
        clear_post_sampling_hooks();
        let calls = Arc::new(AtomicUsize::new(0));
        let seen_messages = Arc::new(Mutex::new(0usize));
        let seen_messages_for_hook = seen_messages.clone();
        let calls_for_hook = calls.clone();
        register_post_sampling_hook(Arc::new(move |context: REPLHookContext| {
            let calls_for_hook = calls_for_hook.clone();
            let seen_messages_for_hook = seen_messages_for_hook.clone();
            Box::pin(async move {
                calls_for_hook.fetch_add(1, Ordering::SeqCst);
                *seen_messages_for_hook.lock().unwrap() = context.messages.len();
                assert_eq!(
                    context
                        .query_source
                        .as_ref()
                        .map(QuerySource::as_api_source),
                    Some("repl_main_thread")
                );
                assert!(context.system_context.contains_key("session_id"));
            })
        }));

        let mut system_context = std::collections::BTreeMap::new();
        system_context.insert("session_id".to_string(), "post-sampling-test".to_string());
        execute_post_sampling_hooks(
            vec![Message::User(crate::types::message::UserMessage {
                uuid: uuid::Uuid::new_v4().to_string(),
                timestamp: chrono::Utc::now(),
                content: vec![crate::types::message::UserContent::Text(
                    "hello".to_string(),
                )],
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
            Vec::new(),
            std::collections::BTreeMap::new(),
            system_context,
            ToolUseContext::default(),
            Some(QuerySource::Prompt),
        )
        .await;

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(*seen_messages.lock().unwrap(), 1);
        clear_post_sampling_hooks();
    }
}
