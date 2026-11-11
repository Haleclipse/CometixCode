use serde::{Deserialize, Serialize};

use super::ids::ToolUseId;

#[derive(Debug, Clone, PartialEq)]
pub enum ToolProgress {
    // `PendingApproval` and `Running { live_output }` are gone: neither had
    // a producer anywhere in the tree, and `Running` existed only to feed the
    // invented `ShellOutputTail` payload (CC's bash progress is the full
    // `bash_progress` object, BashTool.tsx:900-912 — there is no tail-only
    // shape).
    /// Maps to CC `BashProgress` yielded by `runShellCommand`.
    BashProgress {
        tool_use_id: ToolUseId,
        output: String,
        full_output: String,
        elapsed_time_seconds: u64,
        total_lines: usize,
        total_bytes: Option<u64>,
        task_id: Option<String>,
        timeout_ms: Option<u64>,
    },
    /// Maps to CC `tools/WebSearchTool/UI.tsx` `WebSearchProgress`
    /// `query_update` rows forwarded through `ToolCallProgress`.
    WebSearchQueryUpdate {
        tool_use_id: ToolUseId,
        query: String,
    },
    /// Maps to CC `tools/WebSearchTool/UI.tsx` `WebSearchProgress`
    /// `search_results_received` rows forwarded through `ToolCallProgress`.
    WebSearchResultsReceived {
        tool_use_id: ToolUseId,
        query: String,
        result_count: usize,
    },
    /// Maps to CC `agent_progress` — the `AgentTool.tsx` foreground loop
    /// literal at `:1494-1506` (and the metadata-only first message at
    /// `:1084-1092`): `{ message, type: 'agent_progress', prompt, agentId }`.
    ///
    /// CC forwards the WHOLE normalized message and derives every display row
    /// at the renderer (`AgentTool/UI.tsx`); this carrier does the same, so a
    /// render-side field the producer did not anticipate is no longer lost.
    /// `types/tools.ts` is a generated stub whose `AgentToolProgress` omits
    /// `prompt`, so the field list comes from the producer literal ∪ the
    /// consumer reads, not from the declaration.
    AgentProgress {
        /// Maps to CC `ProgressMessage.parentToolUseID` (types/message.ts:114,
        /// written at toolExecution.ts:551 `parentToolUseID: toolUseID`) — the
        /// PARENT Agent row this progress is grouped under
        /// (`utils/messages.ts:1055` `_.parentToolUseID === toolUseID`).
        ///
        /// CC's sibling `ProgressMessage.toolUseID` is a different value the
        /// port does not carry: for subagents AgentTool synthesises
        /// `agent_${assistantMessage.message.id}` (AgentTool.tsx:1085), which
        /// nothing in the render path reads. Explicit seam.
        parent_tool_use_id: ToolUseId,
        /// CC `data.message` — one whole message out of
        /// `normalizeMessages([message])`, i.e. already split to a single
        /// content block (AgentTool.tsx:1483).
        message: Box<crate::types::message::Message>,
        /// CC `data.prompt`. The loop literal sends `''` on purpose
        /// (AgentTool.tsx:1500-1502: "prompt only needed on first progress
        /// message (UI.tsx:624 reads progressMessages[0])").
        prompt: String,
        /// CC `data.agentId` (`syncAgentId`).
        agent_id: String,
    },
    /// Maps to CC `skill_progress` (`SkillTool.ts:250-258`) — the same literal
    /// with a different `type` tag.
    ///
    /// Seam: no Rust producer. Nyan's forked-skill path delegates to
    /// `run_agent`, which emits `agent_progress`; CC's SkillTool owns a second
    /// message loop (`SkillTool.ts:239-261`) that Nyan has not ported.
    SkillProgress {
        /// See [`ToolProgress::AgentProgress::parent_tool_use_id`].
        parent_tool_use_id: ToolUseId,
        message: Box<crate::types::message::Message>,
        /// CC `data.prompt` — SkillTool sends the whole `skillContent` on
        /// EVERY message (`SkillTool.ts:255`), unlike AgentTool's loop.
        prompt: String,
        agent_id: String,
    },
    /// Maps to CC `TaskOutputTool.tsx:275-284` `waiting_for_task` progress.
    TaskOutputWaiting {
        tool_use_id: ToolUseId,
        task_description: String,
        task_type: String,
    },
    Completed {
        tool_use_id: ToolUseId,
        tool_name: String,
        duration: std::time::Duration,
        success: bool,
    },
}

/// Maps to: CC `Tool.ts:450-455` `Tool.mcpInfo`.
///
/// Server and tool names as received from the MCP server (unnormalized).
/// Present on every MCP tool regardless of whether `Tool.name` carries the
/// `mcp__server__` prefix or the unprefixed `NYAN_SDK_MCP_NO_PREFIX`
/// display name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpToolInfo {
    pub server_name: String,
    pub tool_name: String,
}

/// What an MCP tool's `call` closes over instead of being looked up by name.
#[derive(Clone)]
pub enum McpToolConnection {
    /// The connected server the tool was fetched from: `fetchToolsForClient`'s
    /// per-tool closures capture `client` (`services/mcp/client.ts:1767-1988`)
    /// and call through `ensureConnectedClient(client)`. Shared by every tool
    /// of one fetch.
    Connected(std::sync::Arc<crate::services::mcp::types::ConnectedMcpServer>),
    /// `createMcpAuthTool(serverName, config)`'s capture
    /// (`tools/McpAuthTool/McpAuthTool.ts:49-52`): its `call` starts the
    /// server's OAuth flow whatever the client's status is now.
    Authenticate {
        server_name: String,
        config: crate::services::mcp::types::ScopedMcpServerConfig,
    },
}

/// A captured connection is the same one only if it is the same capture, as
/// JS compares the closed-over object by reference.
impl PartialEq for McpToolConnection {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Connected(a), Self::Connected(b)) => std::sync::Arc::ptr_eq(a, b),
            (
                Self::Authenticate {
                    server_name: a_name,
                    config: a_config,
                },
                Self::Authenticate {
                    server_name: b_name,
                    config: b_config,
                },
            ) => a_name == b_name && a_config == b_config,
            _ => false,
        }
    }
}

impl std::fmt::Debug for McpToolConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connected(server) => f
                .debug_tuple("Connected")
                .field(&server.name)
                .field(&server.client)
                .finish(),
            Self::Authenticate { server_name, .. } => {
                f.debug_tuple("Authenticate").field(server_name).finish()
            }
        }
    }
}

/// The server tool's MCP `annotations`, which the per-tool closures of CC's
/// `fetchToolsForClient` read (`services/mcp/client.ts:1795-1809,1972-1976`).
/// A missing hint is `false`, as their `?? false` makes it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpToolAnnotations {
    /// `annotations.title`, which `userFacingName()` prefers to the tool name.
    pub title: Option<String>,
    /// `isReadOnly()`, and `isConcurrencySafe()` with it.
    pub read_only_hint: bool,
    /// `isDestructive()`.
    pub destructive_hint: bool,
    /// `isOpenWorld()`.
    pub open_world_hint: bool,
}

/// Maps to: CC `Tool.ts` `Tool` metadata consumed by `utils/api.ts#toolToAPISchema`.
///
/// This type is the model/API-visible metadata half of CC's `Tool` interface,
/// carried from the tool registry into `services/api/request.rs`. The
/// behavioral half (`Tool.call`) is the `ToolCall` trait in `crate::tool`,
/// implemented by each `tools/<tool>` module.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    /// Maps to CC `Tool.aliases` for backwards-compatible tool lookup.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// The instance's base render of CC `Tool.prompt()` (Tool.ts:518-523).
    ///
    /// No reader consumes this field where CC calls `tool.prompt(...)`: every
    /// such site goes through [`Tool::prompt`], which dispatches registry
    /// built-ins to `ToolCall::prompt(tool, options)`. The field is the
    /// carrier for the INSTANCE-bound prompts that dispatch resolves back to
    /// (MCP server descriptions, `services/mcp/client.ts:1789-1794`;
    /// StructuredOutput overrides, `hookHelpers.ts:60-63`) and the source the
    /// eager schema constructors render (`tools/mod.rs`'s byte-stability sweep
    /// pins the two against each other for every registered tool).
    pub description: String,
    /// Maps to: CC `Tool.inputJSONSchema ?? zodToJsonSchema(tool.inputSchema)`.
    pub input_schema: serde_json::Value,
    /// Maps to CC Tool.inputSchema when an instance overrides the registry Zod schema.
    #[serde(skip)]
    pub input_zod_schema: Option<InputSchema>,
    pub is_mcp: bool,
    /// Maps to: CC `Tool.mcpInfo`, set by `services/mcp/client.ts#fetchToolsForClient`.
    /// Permission rule matching resolves through this rather than `name`, so an
    /// unprefixed MCP tool never shares a rule-match name with the builtin it
    /// shadows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_info: Option<McpToolInfo>,
    /// The MCP tool's annotations; default for every other tool.
    #[serde(skip)]
    pub mcp_annotations: McpToolAnnotations,
    /// Maps to: CC MCP tool `description()` (`client.ts:1786-1788`), the
    /// server's description untruncated. [`Tool::description`] holds what
    /// `prompt()` returns, cut at `MAX_MCP_DESCRIPTION_LENGTH`.
    #[serde(skip)]
    pub mcp_description: String,
    /// What the MCP tool's `call` closes over; `None` for a tool dispatched
    /// by name.
    #[serde(skip)]
    pub mcp_connection: Option<McpToolConnection>,
    /// Maps to: CC `Tool.strict` after model/gate filtering in `toolToAPISchema`.
    pub strict: Option<bool>,
}

/// Instance identity carrier for CC's overridden Tool.inputSchema.
#[derive(Clone, Debug)]
pub struct InputSchema(pub &'static crate::utils::zod::Schema);
impl PartialEq for InputSchema {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0, other.0)
    }
}

impl Tool {
    /// Maps to: CC `Tool.ts:518-523` `prompt(options)` — the model/API-facing
    /// tool description, resolved per read ON THE INSTANCE.
    ///
    /// CC's tool object owns metadata and behavior together, so every reader
    /// writes `await tool.prompt({...})`: `utils/api.ts:171-176`,
    /// `tools/ToolSearchTool/ToolSearchTool.ts:73-84`,
    /// `utils/analyzeContext.ts:652-656`, `entrypoints/mcp.ts:85`. This port
    /// splits that object into this wire instance plus the process-wide
    /// `crate::tool::ToolCall` behavior singleton, so the resolution CC gets
    /// from method dispatch is this function:
    /// - MCP tools return the instance's stored (truncated) server description
    ///   (`services/mcp/client.ts:1789-1794`). The `is_mcp` arm comes FIRST so
    ///   an MCP tool that shadows a built-in name never renders the built-in's
    ///   prompt;
    /// - registry built-ins render lazily through `ToolCall::prompt`;
    /// - anything else (a tool with no registered behavior, e.g. a hand-built
    ///   test instance or `StructuredOutput`'s `hookHelpers.ts:60-63`
    ///   per-instance override) falls back to the instance's own text, which
    ///   is where CC's instance-bound `prompt()` closures read from too.
    ///
    /// Sync per PORTING.md § Node 异步模型 A4 — see
    /// [`crate::tool::ToolPromptOptions`] for the tier evidence.
    pub fn prompt(&self, options: &crate::tool::ToolPromptOptions<'_>) -> String {
        if self.is_mcp {
            return self.description.clone();
        }
        match crate::services::tools::tool_execution::find_tool_call(&self.name) {
            Some(call) => call.prompt(self, options),
            None => self.description.clone(),
        }
    }

    /// Maps to: CC MCP tool `userFacingName()` (`client.ts:1972-1976`):
    /// `` `${client.name} - ${annotations.title || tool.name} (MCP)` ``.
    /// `None` for a tool that is not from an MCP server.
    ///
    /// @nyan: CC's Claude in Chrome override (`client.ts:1977-1982`) is not
    /// shipped; every MCP server's tools use this generic name.
    pub fn mcp_user_facing_name(&self) -> Option<String> {
        let info = self.mcp_info.as_ref()?;
        Some(crate::services::mcp::client::mcp_tool_user_facing_name(
            &info.server_name,
            &crate::services::mcp::client::mcp_tool_display_name(
                self.mcp_annotations.title.as_deref(),
                &info.tool_name,
            ),
        ))
    }
}

pub type ToolDefinition = Tool;

/// Maps to: CC `Tool.ts` `toolMatchesName(...)`.
pub fn tool_matches_name(tool: &Tool, name: &str) -> bool {
    tool.name == name || tool.aliases.iter().any(|alias| alias == name)
}

/// Maps to: CC `Tool.ts` `findToolByName(...)`.
pub fn find_tool_by_name<'a>(tools: &'a [Tool], name: &str) -> Option<&'a Tool> {
    tools.iter().find(|tool| tool_matches_name(tool, name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    fn aliased_tool() -> Tool {
        Tool {
            name: "SendUserMessage".to_string(),
            aliases: vec!["Brief".to_string()],
            description: "Send a message to the user".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
            strict: Some(true),
            ..Default::default()
        }
    }

    #[test]
    fn tool_matches_name_checks_primary_name_and_aliases_like_official() {
        let tool = aliased_tool();

        assert!(tool_matches_name(&tool, "SendUserMessage"));
        assert!(tool_matches_name(&tool, "Brief"));
        assert!(!tool_matches_name(&tool, "Other"));
    }

    #[test]
    fn find_tool_by_name_checks_aliases_like_official() {
        let tools = vec![aliased_tool()];

        let found = find_tool_by_name(&tools, "Brief").expect("alias should match");

        assert_eq!(found.name, "SendUserMessage");
    }

    fn prompt_options<'a>(
        tool_permission_context: &'a crate::tool::ToolPermissionContext,
        agents: &'a [crate::tools::agent_tool::load_agents_dir::AgentDefinition],
    ) -> crate::tool::ToolPromptOptions<'a> {
        crate::tool::ToolPromptOptions {
            tool_permission_context,
            tools: &[],
            agents,
        }
    }

    /// CC `Tool.ts:518-523` is an instance member, so `tool.prompt(...)` on a
    /// registry built-in renders the CURRENT options, never a field frozen at
    /// schema-construction time (`AgentTool.tsx:339-371` reads the permission
    /// context and the agent list).
    ///
    /// Old shape: `utils/api.rs#tool_api_description` owned this dispatch
    /// privately, so the other two CC `tool.prompt(...)` readers
    /// (`ToolSearchTool.ts:241/262`, `analyzeContext.ts:652`) had no way to
    /// reach it and read the eager `description` field instead.
    #[test]
    fn tool_prompt_dispatches_registry_built_ins_to_the_lazy_member() {
        let _env_guard = TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::utils::process_env::remove("NYAN_AGENT_LIST_IN_MESSAGES");
        crate::utils::process_env::remove("NYAN_COORDINATOR_MODE");
        let agents =
            vec![crate::tools::agent_tool::load_agents_dir::AgentDefinition::new(
                "reviewer",
                "review code",
                crate::tools::agent_tool::load_agents_dir::AgentDefinitionSource::ProjectSettings,
            )];
        let tool = Tool {
            name: "Agent".to_string(),
            // Deliberately not the real listing: whatever comes back proves
            // which source was read.
            description: "STALE-EAGER-FIELD".to_string(),
            ..Default::default()
        };

        let open = crate::tool::ToolPermissionContext::default();
        let rendered = tool.prompt(&prompt_options(&open, &agents));
        assert!(!rendered.contains("STALE-EAGER-FIELD"));
        assert!(rendered.contains("- reviewer: review code"));

        // ... and the options actually reach the member: an `Agent(reviewer)`
        // deny rule drops the type (`AgentTool.tsx:359-363`).
        let mut denied = crate::tool::ToolPermissionContext::default();
        denied.always_deny_rules.insert(
            crate::types::permissions::PermissionRuleSource::LocalSettings,
            vec![crate::types::permissions::PermissionRuleValue::new(
                "Agent",
                Some("reviewer".to_string()),
            )],
        );
        assert!(
            !tool
                .prompt(&prompt_options(&denied, &agents))
                .contains("- reviewer:")
        );
    }

    /// CC's MCP tools carry their own `async prompt()` returning the truncated
    /// server description (`services/mcp/client.ts:1789-1794`), which this port
    /// stores in `description`. The `is_mcp` arm therefore comes FIRST, so an
    /// MCP tool whose (unprefixed) name shadows a built-in never renders the
    /// built-in's prompt. A name with no registered behavior falls back to the
    /// same field.
    #[test]
    fn tool_prompt_reads_the_instance_for_mcp_and_unregistered_tools() {
        let context = crate::tool::ToolPermissionContext::default();
        let options = prompt_options(&context, &[]);

        let shadowing_mcp_tool = Tool {
            // The built-in `Read` is registered; `is_mcp` must win anyway.
            name: "Read".to_string(),
            description: "server-provided description".to_string(),
            is_mcp: true,
            ..Default::default()
        };
        assert_eq!(
            shadowing_mcp_tool.prompt(&options),
            "server-provided description"
        );

        let unregistered = Tool {
            name: "NotARegisteredTool".to_string(),
            description: "instance-bound text".to_string(),
            ..Default::default()
        };
        assert_eq!(unregistered.prompt(&options), "instance-bound text");
    }
}
