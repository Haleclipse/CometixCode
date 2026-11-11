//! API-query backed post-sampling hook helper.
//! Maps to: CC `utils/hooks/apiQueryHookHelper.ts`.
//!
//! The official helper builds a post-sampling hook that optionally issues a
//! non-streaming Claude query, parses its text response, and reports a typed
//! success/error result through caller-provided callbacks. Nyan keeps the
//! same boundary and injects the query executor for tests/safety; the default
//! executor calls `services/api/request.rs#query_model_without_streaming`.

use super::post_sampling_hooks::{PostSamplingHook, REPLHookContext};
use crate::services::api::request::{Options, SystemPrompt, query_model_without_streaming};
use crate::tool::ToolUseContext;
use crate::types::message::{AssistantContent, AssistantMessage, Message};
use crate::types::tools::Tool;
use crate::utils::thinking::ThinkingConfig;
use futures::future::BoxFuture;
use std::sync::Arc;

/// Maps to: CC `ApiQueryHookContext`.
#[derive(Clone, Debug)]
pub struct ApiQueryHookContext {
    pub repl: REPLHookContext,
    pub query_message_count: Option<usize>,
}

impl ApiQueryHookContext {
    pub fn tool_use_context(&self) -> &ToolUseContext {
        &self.repl.tool_use_context
    }
}

/// Query request assembled by `create_api_query_hook*` before calling the model.
/// Maps to the `queryModelWithoutStreaming({...})` argument object in CC.
#[derive(Clone, Debug)]
pub struct ApiQueryRequest {
    pub query_name: String,
    pub messages: Vec<Message>,
    pub system_prompt: SystemPrompt,
    pub tools: Vec<Tool>,
    pub model: String,
    pub is_non_interactive_session: bool,
    pub has_append_system_prompt: bool,
    pub agent_id: Option<String>,
}

/// Response projection needed by the helper.
///
/// CC receives `response.message.id`; Nyan's current `AssistantMessage`
/// type does not preserve provider message IDs, so injected executors can
/// supply one and the default executor uses the hook UUID as a stable fallback.
#[derive(Clone, Debug)]
pub struct ApiQueryAssistantResponse {
    pub message: AssistantMessage,
    pub message_id: Option<String>,
}

/// Maps to: CC `ApiQueryResult<TResult>`.
#[derive(Clone, Debug, PartialEq)]
pub enum ApiQueryResult<TResult> {
    Success {
        query_name: String,
        result: TResult,
        message_id: String,
        model: String,
        uuid: String,
    },
    Error {
        query_name: String,
        error: String,
        uuid: String,
    },
}

/// Maps to: CC `ApiQueryHookConfig<TResult>`.
pub struct ApiQueryHookConfig<TResult> {
    pub name: String,
    pub should_run: Arc<dyn Fn(&ApiQueryHookContext) -> BoxFuture<'static, bool> + Send + Sync>,
    pub build_messages: Arc<dyn Fn(&ApiQueryHookContext) -> Vec<Message> + Send + Sync>,
    pub system_prompt: Option<String>,
    pub use_tools: Option<bool>,
    pub parse_response:
        Arc<dyn Fn(&str, &ApiQueryHookContext) -> Result<TResult, String> + Send + Sync>,
    pub log_result: Arc<dyn Fn(ApiQueryResult<TResult>, &ApiQueryHookContext) + Send + Sync>,
    pub get_model: Arc<dyn Fn(&ApiQueryHookContext) -> String + Send + Sync>,
}

pub type ApiQueryExecutor = Arc<
    dyn Fn(ApiQueryRequest) -> BoxFuture<'static, anyhow::Result<ApiQueryAssistantResponse>>
        + Send
        + Sync,
>;

fn assistant_text_content(message: &AssistantMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|content| match content {
            AssistantContent::Text(text) => Some(text.as_str()),
            AssistantContent::Advisor { content, .. } => content.text(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Maps to: CC `createApiQueryHook(config)` with query executor injected.
pub fn create_api_query_hook_with_executor<TResult>(
    config: ApiQueryHookConfig<TResult>,
    executor: ApiQueryExecutor,
) -> PostSamplingHook
where
    TResult: Send + 'static,
{
    let config = Arc::new(config);
    Arc::new(move |context: REPLHookContext| {
        let config = Arc::clone(&config);
        let executor = Arc::clone(&executor);
        Box::pin(async move {
            let mut hook_context = ApiQueryHookContext {
                repl: context,
                query_message_count: None,
            };

            if !(config.should_run)(&hook_context).await {
                return;
            }

            let uuid = uuid::Uuid::new_v4().to_string();
            let messages = (config.build_messages)(&hook_context);
            hook_context.query_message_count = Some(messages.len());

            let system_prompt = config
                .system_prompt
                .as_ref()
                .map(|prompt| vec![prompt.clone()])
                .unwrap_or_else(|| hook_context.repl.system_prompt.clone());

            let tools = if config.use_tools.unwrap_or(true) {
                hook_context.repl.tool_use_context.tools.clone()
            } else {
                Vec::new()
            };

            let model = (config.get_model)(&hook_context);
            let request = ApiQueryRequest {
                query_name: config.name.clone(),
                messages,
                system_prompt,
                tools,
                model: model.clone(),
                is_non_interactive_session: hook_context
                    .repl
                    .tool_use_context
                    .is_non_interactive_session,
                has_append_system_prompt: false,
                agent_id: hook_context.repl.tool_use_context.agent_id.clone(),
            };

            let response = match executor(request).await {
                Ok(response) => response,
                Err(error) => {
                    (config.log_result)(
                        ApiQueryResult::Error {
                            query_name: config.name.clone(),
                            error: error.to_string(),
                            uuid,
                        },
                        &hook_context,
                    );
                    return;
                }
            };

            let content = assistant_text_content(&response.message).trim().to_string();
            match (config.parse_response)(&content, &hook_context) {
                Ok(result) => (config.log_result)(
                    ApiQueryResult::Success {
                        query_name: config.name.clone(),
                        result,
                        message_id: response.message_id.unwrap_or_else(|| uuid.clone()),
                        model,
                        uuid,
                    },
                    &hook_context,
                ),
                Err(error) => (config.log_result)(
                    ApiQueryResult::Error {
                        query_name: config.name.clone(),
                        error,
                        uuid,
                    },
                    &hook_context,
                ),
            }
        })
    })
}

/// Maps to: CC `createApiQueryHook(config)` using the production Claude API
/// executor. Calling this registers no hook by itself and performs no network
/// I/O until the returned post-sampling hook runs.
pub fn create_api_query_hook<TResult>(config: ApiQueryHookConfig<TResult>) -> PostSamplingHook
where
    TResult: Send + 'static,
{
    create_api_query_hook_with_executor(config, Arc::new(default_api_query_executor))
}

fn default_api_query_executor(
    request: ApiQueryRequest,
) -> BoxFuture<'static, anyhow::Result<ApiQueryAssistantResponse>> {
    Box::pin(async move {
        let mut options = Options::new(request.model.clone(), request.query_name.clone());
        options.is_non_interactive_session = request.is_non_interactive_session;
        options.has_append_system_prompt = request.has_append_system_prompt;
        options.temperature_override = Some(0.0);
        options.agent_id = request.agent_id.clone().map(crate::types::ids::AgentId);
        let message = query_model_without_streaming(
            &request.messages,
            &request.system_prompt,
            &ThinkingConfig::Disabled,
            &request.tools,
            &options,
        )
        .await?;
        Ok(ApiQueryAssistantResponse {
            message,
            message_id: None,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::query_source::QuerySource;
    use crate::services::api::request::SystemPrompt;
    use crate::types::message::{AssistantMessage, UserContent, UserMessage};
    use chrono::Utc;
    use std::sync::Mutex;

    fn user_message(text: &str) -> Message {
        Message::User(UserMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            content: vec![UserContent::Text(text.to_string())],
            is_compact_summary: false,
            plan_content: None,
            image_paste_ids: None,
            is_visible_in_transcript_only: false,
            mcp_meta: None,
            source_tool_assistant_uuid: None,
            permission_mode: None,
            origin: None,
            summarize_metadata: None,
        })
    }

    fn assistant_message(text: &str) -> AssistantMessage {
        AssistantMessage {
            uuid: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            content: vec![AssistantContent::Text(text.to_string())],
            model: Some("model".to_string()),
            stop_reason: None,
            usage: None,
        }
    }

    fn repl_context() -> REPLHookContext {
        REPLHookContext {
            messages: vec![user_message("hello")],
            system_prompt: vec!["default system".to_string()],
            user_context: Default::default(),
            system_context: Default::default(),
            tool_use_context: ToolUseContext::default(),
            query_source: Some(QuerySource::Prompt),
        }
    }

    #[tokio::test]
    async fn should_run_false_skips_query() {
        let calls = Arc::new(Mutex::new(0usize));
        let calls_for_executor = Arc::clone(&calls);
        let hook = create_api_query_hook_with_executor(
            ApiQueryHookConfig::<String> {
                name: "skip_query".to_string(),
                should_run: Arc::new(|_| Box::pin(async { false })),
                build_messages: Arc::new(|_| vec![user_message("should not build")]),
                system_prompt: None,
                use_tools: None,
                parse_response: Arc::new(|content, _| Ok(content.to_string())),
                log_result: Arc::new(|_, _| panic!("should not log")),
                get_model: Arc::new(|_| "model".to_string()),
            },
            Arc::new(move |_| {
                let calls = Arc::clone(&calls_for_executor);
                Box::pin(async move {
                    *calls.lock().unwrap() += 1;
                    Ok(ApiQueryAssistantResponse {
                        message: assistant_message("unused"),
                        message_id: Some("msg".to_string()),
                    })
                })
            }),
        );

        hook(repl_context()).await;
        assert_eq!(*calls.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn success_builds_request_parses_content_and_logs_result() {
        let seen_request = Arc::new(Mutex::new(None::<ApiQueryRequest>));
        let seen_request_for_executor = Arc::clone(&seen_request);
        let logged = Arc::new(Mutex::new(None::<ApiQueryResult<Vec<String>>>));
        let logged_for_hook = Arc::clone(&logged);
        let hook = create_api_query_hook_with_executor(
            ApiQueryHookConfig {
                name: "skill_improvement".to_string(),
                should_run: Arc::new(|context| {
                    assert_eq!(
                        context.repl.query_source.as_ref().unwrap().as_api_source(),
                        "repl_main_thread"
                    );
                    Box::pin(async { true })
                }),
                build_messages: Arc::new(|context| {
                    assert!(context.query_message_count.is_none());
                    vec![user_message("analyze")]
                }),
                system_prompt: Some("override system".to_string()),
                use_tools: Some(false),
                parse_response: Arc::new(|content, context| {
                    assert_eq!(context.query_message_count, Some(1));
                    Ok(content
                        .split(',')
                        .map(|part| part.trim().to_string())
                        .collect())
                }),
                log_result: Arc::new(move |result, context| {
                    assert_eq!(context.query_message_count, Some(1));
                    *logged_for_hook.lock().unwrap() = Some(result);
                }),
                get_model: Arc::new(|context| {
                    assert_eq!(context.query_message_count, Some(1));
                    "small-fast".to_string()
                }),
            },
            Arc::new(move |request| {
                let seen_request = Arc::clone(&seen_request_for_executor);
                Box::pin(async move {
                    *seen_request.lock().unwrap() = Some(request);
                    Ok(ApiQueryAssistantResponse {
                        message: assistant_message("one, two"),
                        message_id: Some("msg-1".to_string()),
                    })
                })
            }),
        );

        hook(repl_context()).await;

        let request = seen_request.lock().unwrap().clone().expect("request");
        assert_eq!(request.query_name, "skill_improvement");
        assert_eq!(request.messages.len(), 1);
        assert_eq!(
            request.system_prompt,
            vec!["override system".to_string()] as SystemPrompt
        );
        assert!(request.tools.is_empty());
        assert_eq!(request.model, "small-fast");
        let logged = logged.lock().unwrap().clone().expect("logged result");
        assert!(matches!(
            logged,
            ApiQueryResult::Success { result, message_id, model, .. }
                if result == vec!["one".to_string(), "two".to_string()]
                    && message_id == "msg-1"
                    && model == "small-fast"
        ));
    }

    #[tokio::test]
    async fn parse_errors_are_reported_as_error_results() {
        let logged = Arc::new(Mutex::new(None::<ApiQueryResult<String>>));
        let logged_for_hook = Arc::clone(&logged);
        let hook = create_api_query_hook_with_executor(
            ApiQueryHookConfig {
                name: "parse_error".to_string(),
                should_run: Arc::new(|_| Box::pin(async { true })),
                build_messages: Arc::new(|_| vec![user_message("msg")]),
                system_prompt: None,
                use_tools: None,
                parse_response: Arc::new(|_, _| Err("bad response".to_string())),
                log_result: Arc::new(move |result, _| {
                    *logged_for_hook.lock().unwrap() = Some(result);
                }),
                get_model: Arc::new(|_| "model".to_string()),
            },
            Arc::new(|_| {
                Box::pin(async {
                    Ok(ApiQueryAssistantResponse {
                        message: assistant_message("bad"),
                        message_id: Some("msg".to_string()),
                    })
                })
            }),
        );

        hook(repl_context()).await;
        let logged = logged.lock().unwrap().clone().expect("logged");
        assert!(matches!(
            logged,
            ApiQueryResult::Error { query_name, error, .. }
                if query_name == "parse_error" && error == "bad response"
        ));
    }

    #[tokio::test]
    async fn query_errors_are_reported_as_error_results() {
        let logged = Arc::new(Mutex::new(None::<ApiQueryResult<String>>));
        let logged_for_hook = Arc::clone(&logged);
        let hook = create_api_query_hook_with_executor(
            ApiQueryHookConfig {
                name: "query_error".to_string(),
                should_run: Arc::new(|_| Box::pin(async { true })),
                build_messages: Arc::new(|_| vec![user_message("msg")]),
                system_prompt: None,
                use_tools: None,
                parse_response: Arc::new(|content, _| Ok(content.to_string())),
                log_result: Arc::new(move |result, _| {
                    *logged_for_hook.lock().unwrap() = Some(result);
                }),
                get_model: Arc::new(|_| "model".to_string()),
            },
            Arc::new(|_| Box::pin(async { Err(anyhow::anyhow!("network disabled")) })),
        );

        hook(repl_context()).await;
        let logged = logged.lock().unwrap().clone().expect("logged");
        assert!(matches!(
            logged,
            ApiQueryResult::Error { query_name, error, .. }
                if query_name == "query_error" && error == "network disabled"
        ));
    }
}
