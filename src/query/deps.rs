//! Query dependency seam.
//! Maps to official `query/deps.ts`.

use crate::constants::query_source::QuerySource;
use crate::services::api::request::{Options, SystemPrompt};
use crate::services::compact::auto_compact::{
    AutoCompactCacheSafeParams, AutoCompactTrackingState, auto_compact_if_needed,
};
use crate::services::compact::micro_compact::microcompact_messages;
use crate::tool::ToolPermissionContext;
use crate::tool::ToolUseContext;
use crate::types::message::Message;
use crate::types::tools::Tool;
use crate::utils::thinking::ThinkingConfig;
use futures::future::BoxFuture;

pub type CallModelStream =
    tokio::sync::mpsc::Receiver<crate::services::api::request::QueryModelStreamItem>;
pub type CallModelStreamFuture = BoxFuture<'static, anyhow::Result<CallModelStream>>;

pub use crate::services::compact::auto_compact::AutocompactResult;
pub use crate::services::compact::micro_compact::MicrocompactResult;

/// Maps to the argument object passed from CC `query.ts` into
/// `QueryDeps.callModel`, whose concrete production value is
/// `services/api/claude.ts#queryModelWithStreaming(...)`.
#[derive(Clone, Debug)]
pub struct CallModelRequest {
    pub messages: Vec<Message>,
    pub system_prompt: SystemPrompt,
    pub thinking_config: ThinkingConfig,
    pub tools: Vec<Tool>,
    pub options: Options,
    /// Rust-owned equivalent of CC `options.getToolPermissionContext()`.
    pub permission_context: ToolPermissionContext,
    pub query_source: QuerySource,
}

/// I/O dependencies for [`query`](crate::query::query).
pub trait QueryDeps: Clone + Send + Sync + 'static {
    /// Rust streaming counterpart of upstream `callModel: typeof queryModelWithStreaming`.
    /// Maps to: CC `query/deps.ts` `QueryDeps.callModel`.
    fn call_model(&self, _request: CallModelRequest) -> CallModelStreamFuture {
        Box::pin(async {
            Err(anyhow::anyhow!(
                "streaming callModel is not implemented for this QueryDeps"
            ))
        })
    }

    /// Maps to upstream `microcompact: typeof microcompactMessages`.
    fn microcompact(
        &self,
        messages_for_query: Vec<Message>,
        tool_use_context: &ToolUseContext,
        query_source: &QuerySource,
    ) -> MicrocompactResult {
        microcompact_messages(messages_for_query, tool_use_context, query_source)
    }

    /// Maps to upstream `autocompact: typeof autoCompactIfNeeded`.
    fn autocompact(
        &self,
        messages_for_query: Vec<Message>,
        tool_use_context: &ToolUseContext,
        cache_safe_params: AutoCompactCacheSafeParams,
        query_source: &QuerySource,
        tracking: Option<AutoCompactTrackingState>,
        snip_tokens_freed: i64,
    ) -> BoxFuture<'static, AutocompactResult> {
        let tool_use_context = tool_use_context.clone();
        let query_source = query_source.clone();
        Box::pin(async move {
            auto_compact_if_needed(
                messages_for_query,
                &tool_use_context,
                cache_safe_params,
                &query_source,
                tracking,
                snip_tokens_freed,
            )
            .await
        })
    }

    /// Maps to upstream `uuid: () => string`.
    fn uuid(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }
}

/// Maps to official `productionDeps()`.
pub fn production_deps() -> ProductionDeps {
    ProductionDeps
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProductionDeps;

impl QueryDeps for ProductionDeps {
    fn call_model(&self, request: CallModelRequest) -> CallModelStreamFuture {
        Box::pin(async move {
            crate::services::api::request::query_model_with_streaming(
                &request.messages,
                &request.system_prompt,
                &request.thinking_config,
                &request.tools,
                &request.options,
            )
            .await
        })
    }
}
