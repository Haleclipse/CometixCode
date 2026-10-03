//! Maps to: CC `components/agents/generateAgent.ts:1-197`.
//!
//! The component boundary owns the official prompt construction and response
//! parsing. Network/model execution is dependency-injected so importing or
//! testing this module cannot initiate an API request. Telemetry is omitted by
//! project policy.

use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Cancellation handle passed to an injected generation backend. Cancelling
/// drops the in-flight future even when a backend does not inspect the token.
struct AgentGenerationCancellationInner {
    sender: async_channel::Sender<()>,
    receiver: async_channel::Receiver<()>,
}

#[derive(Clone)]
pub struct AgentGenerationCancellation {
    inner: Arc<AgentGenerationCancellationInner>,
}

impl AgentGenerationCancellation {
    pub fn new() -> Self {
        let (sender, receiver) = async_channel::bounded(1);
        Self {
            inner: Arc::new(AgentGenerationCancellationInner { sender, receiver }),
        }
    }

    pub fn cancel(&self) {
        let _ = self.inner.sender.try_send(());
    }

    pub fn is_cancelled(&self) -> bool {
        !self.inner.receiver.is_empty()
    }
}

impl Default for AgentGenerationCancellation {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedAgent {
    pub identifier: String,
    pub when_to_use: String,
    pub system_prompt: String,
}

pub type AgentGeneratorFuture =
    Pin<Box<dyn Future<Output = Result<GeneratedAgent, String>> + Send>>;

type AgentGeneratorHandler =
    dyn Fn(String, AgentGenerationCancellation) -> AgentGeneratorFuture + Send + Sync;

/// Runtime injection point used by `GenerateStep`; no network backend is
/// installed by the external build unless the application explicitly supplies one.
#[derive(Clone)]
pub struct AgentGeneratorRuntime {
    handler: Arc<AgentGeneratorHandler>,
}

impl AgentGeneratorRuntime {
    /// Compatibility constructor for backends that rely only on future-drop
    /// cancellation (for example, an awaited HTTP request).
    pub fn new(handler: impl Fn(String) -> AgentGeneratorFuture + Send + Sync + 'static) -> Self {
        Self {
            handler: Arc::new(move |prompt, _cancellation| handler(prompt)),
        }
    }

    /// Installs a backend that can additionally inspect the cancellation token.
    pub fn new_abortable(
        handler: impl Fn(String, AgentGenerationCancellation) -> AgentGeneratorFuture
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            handler: Arc::new(handler),
        }
    }

    pub async fn generate(&self, prompt: String) -> Result<GeneratedAgent, String> {
        self.generate_with_cancellation(prompt, AgentGenerationCancellation::new())
            .await
    }

    pub async fn generate_with_cancellation(
        &self,
        prompt: String,
        cancellation: AgentGenerationCancellation,
    ) -> Result<GeneratedAgent, String> {
        use futures::future::{Either, select};

        if cancellation.is_cancelled() {
            return Err("Generation cancelled".to_string());
        }
        let generation = (self.handler)(prompt, cancellation.clone());
        let cancelled = Box::pin(cancellation.inner.receiver.recv());
        match select(generation, cancelled).await {
            Either::Left((result, _)) => result,
            Either::Right((_cancelled, _generation)) => Err("Generation cancelled".to_string()),
        }
    }
}

pub const AGENT_CREATION_SYSTEM_PROMPT: &str = r###"You are an elite AI agent architect specializing in crafting high-performance agent configurations. Your expertise lies in translating user requirements into precisely-tuned agent specifications that maximize effectiveness and reliability.

**Important Context**: You may have access to project-specific instructions from CLAUDE.md files and other context that may include coding standards, project structure, and custom requirements. Consider this context when creating agents to ensure they align with the project's established patterns and practices.

When a user describes what they want an agent to do, you will:

1. **Extract Core Intent**: Identify the fundamental purpose, key responsibilities, and success criteria for the agent. Look for both explicit requirements and implicit needs. Consider any project-specific context from CLAUDE.md files. For agents that are meant to review code, you should assume that the user is asking to review recently written code and not the whole codebase, unless the user has explicitly instructed you otherwise.

2. **Design Expert Persona**: Create a compelling expert identity that embodies deep domain knowledge relevant to the task. The persona should inspire confidence and guide the agent's decision-making approach.

3. **Architect Comprehensive Instructions**: Develop a system prompt that:
   - Establishes clear behavioral boundaries and operational parameters
   - Provides specific methodologies and best practices for task execution
   - Anticipates edge cases and provides guidance for handling them
   - Incorporates any specific requirements or preferences mentioned by the user
   - Defines output format expectations when relevant
   - Aligns with project-specific coding standards and patterns from CLAUDE.md

4. **Optimize for Performance**: Include:
   - Decision-making frameworks appropriate to the domain
   - Quality control mechanisms and self-verification steps
   - Efficient workflow patterns
   - Clear escalation or fallback strategies

5. **Create Identifier**: Design a concise, descriptive identifier that:
   - Uses lowercase letters, numbers, and hyphens only
   - Is typically 2-4 words joined by hyphens
   - Clearly indicates the agent's primary function
   - Is memorable and easy to type
   - Avoids generic terms like "helper" or "assistant"

6 **Example agent descriptions**:
  - in the 'whenToUse' field of the JSON object, you should include examples of when this agent should be used.
  - examples should be of the form:
    - <example>
      Context: The user is creating a test-runner agent that should be called after a logical chunk of code is written.
      user: "Please write a function that checks if a number is prime"
      assistant: "Here is the relevant function: "
      <function call omitted for brevity only for this example>
      <commentary>
      Since a significant piece of code was written, use the Agent tool to launch the test-runner agent to run the tests.
      </commentary>
      assistant: "Now let me use the test-runner agent to run the tests"
    </example>
    - <example>
      Context: User is creating an agent to respond to the word "hello" with a friendly jok.
      user: "Hello"
      assistant: "I'm going to use the Agent tool to launch the greeting-responder agent to respond with a friendly joke"
      <commentary>
      Since the user is greeting, use the greeting-responder agent to respond with a friendly joke. 
      </commentary>
    </example>
  - If the user mentioned or implied that the agent should be used proactively, you should include examples of this.
- NOTE: Ensure that in the examples, you are making the assistant use the Agent tool and not simply respond directly to the task.

Your output must be a valid JSON object with exactly these fields:
{
  "identifier": "A unique, descriptive identifier using lowercase letters, numbers, and hyphens (e.g., 'test-runner', 'api-docs-writer', 'code-formatter')",
  "whenToUse": "A precise, actionable description starting with 'Use this agent when...' that clearly defines the triggering conditions and use cases. Ensure you include examples as described above.",
  "systemPrompt": "The complete system prompt that will govern the agent's behavior, written in second person ('You are...', 'You will...') and structured for maximum clarity and effectiveness"
}

Key principles for your system prompts:
- Be specific rather than generic - avoid vague instructions
- Include concrete examples when they would clarify behavior
- Balance comprehensiveness with clarity - every instruction should add value
- Ensure the agent has enough context to handle variations of the core task
- Make the agent proactive in seeking clarification when needed
- Build in quality assurance and self-correction mechanisms

Remember: The agents you create should be autonomous experts capable of handling their designated tasks with minimal additional guidance. Your system prompts are their complete operational manual.
"###;

pub const AGENT_MEMORY_INSTRUCTIONS: &str = r###"

7. **Agent Memory Instructions**: If the user mentions "memory", "remember", "learn", "persist", or similar concepts, OR if the agent would benefit from building up knowledge across conversations (e.g., code reviewers learning patterns, architects learning codebase structure, etc.), include domain-specific memory update instructions in the systemPrompt.

   Add a section like this to the systemPrompt, tailored to the agent's specific domain:

   "**Update your agent memory** as you discover [domain-specific items]. This builds up institutional knowledge across conversations. Write concise notes about what you found and where.

   Examples of what to record:
   - [domain-specific item 1]
   - [domain-specific item 2]
   - [domain-specific item 3]"

   Examples of domain-specific memory instructions:
   - For a code-reviewer: "Update your agent memory as you discover code patterns, style conventions, common issues, and architectural decisions in this codebase."
   - For a test-runner: "Update your agent memory as you discover test patterns, common failure modes, flaky tests, and testing best practices."
   - For an architect: "Update your agent memory as you discover codepaths, library locations, key architectural decisions, and component relationships."
   - For a documentation writer: "Update your agent memory as you discover documentation patterns, API structures, and terminology conventions."

   The memory instructions should be specific to what the agent would naturally learn while performing its core tasks.
"###;

pub fn build_agent_generation_user_prompt(
    user_prompt: &str,
    existing_identifiers: &[String],
) -> String {
    let existing = if existing_identifiers.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nIMPORTANT: The following identifiers already exist and must NOT be used: {}",
            existing_identifiers.join(", ")
        )
    };
    format!(
        "Create an agent configuration based on this request: \"{user_prompt}\".{existing}\n  Return ONLY the JSON object, no other text."
    )
}

pub fn agent_creation_system_prompt(auto_memory_enabled: bool) -> String {
    if auto_memory_enabled {
        format!("{AGENT_CREATION_SYSTEM_PROMPT}{AGENT_MEMORY_INSTRUCTIONS}")
    } else {
        AGENT_CREATION_SYSTEM_PROMPT.to_string()
    }
}

pub fn parse_generated_agent_response(response_text: &str) -> Result<GeneratedAgent, String> {
    let trimmed = response_text.trim();
    let direct = serde_json::from_str::<GeneratedAgent>(trimmed);
    let parsed = match direct {
        Ok(value) => value,
        Err(_) => {
            let start = trimmed
                .find('{')
                .ok_or_else(|| "No JSON object found in response".to_string())?;
            let end = trimmed
                .rfind('}')
                .filter(|end| *end >= start)
                .ok_or_else(|| "No JSON object found in response".to_string())?;
            serde_json::from_str::<GeneratedAgent>(&trimmed[start..=end])
                .map_err(|error| format!("Invalid agent configuration generated: {error}"))?
        }
    };
    if parsed.identifier.trim().is_empty()
        || parsed.when_to_use.trim().is_empty()
        || parsed.system_prompt.trim().is_empty()
    {
        return Err("Invalid agent configuration generated".to_string());
    }
    Ok(parsed)
}

/// Safe dependency-injected equivalent of the official query orchestration.
pub async fn generate_agent_with_backend<F, Fut>(
    user_prompt: &str,
    existing_identifiers: &[String],
    auto_memory_enabled: bool,
    query: F,
) -> Result<GeneratedAgent, String>
where
    F: FnOnce(String, String) -> Fut,
    Fut: Future<Output = Result<String, String>>,
{
    let prompt = build_agent_generation_user_prompt(user_prompt, existing_identifiers);
    let system = agent_creation_system_prompt(auto_memory_enabled);
    let response = query(prompt, system).await?;
    parse_generated_agent_response(&response)
}

/// Production backend installed at the App root. Construction is side-effect
/// free; a model request happens only after the user explicitly submits the
/// generation step. The official analytics event is intentionally omitted.
pub fn production_agent_generator_runtime() -> AgentGeneratorRuntime {
    AgentGeneratorRuntime::new_abortable(|prompt, _cancellation| {
        Box::pin(async move {
            let auto_memory_enabled = crate::memdir::paths::is_auto_memory_enabled();
            generate_agent_with_backend(
                &prompt,
                &[],
                auto_memory_enabled,
                |user, system| async move {
                    use crate::services::api::claude::{Options, query_model_without_streaming};
                    use crate::types::message::{
                        AssistantContent, Message, UserContent, UserMessage,
                    };
                    use crate::utils::thinking::ThinkingConfig;

                    let message = Message::User(UserMessage {
                        uuid: uuid::Uuid::new_v4().to_string(),
                        timestamp: chrono::Utc::now(),
                        content: vec![UserContent::Text(user)],
                        is_compact_summary: false,
                        plan_content: None,
                        image_paste_ids: None,
                        is_visible_in_transcript_only: false,
                        mcp_meta: None,
                        source_tool_assistant_uuid: None,
                        permission_mode: None,
                        origin: None,
                        summarize_metadata: None,
                    });
                    let messages = crate::utils::api::prepend_user_context(
                        vec![message],
                        &crate::context::get_user_context(),
                    );
                    let mut options = Options::new(
                        crate::utils::model::model::get_main_loop_model(),
                        "agent_creation".to_string(),
                    );
                    options.temperature_override = Some(0.0);
                    let response = query_model_without_streaming(
                        &messages,
                        &vec![system],
                        &ThinkingConfig::Disabled,
                        &[],
                        &options,
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                    let text = response
                        .content
                        .into_iter()
                        .filter_map(|block| match block {
                            AssistantContent::Text(text) => Some(text),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    if text.is_empty() {
                        Err("No assistant message found".to_string())
                    } else {
                        Ok(text)
                    }
                },
            )
            .await
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_lists_existing_identifiers_only_when_present() {
        let plain = build_agent_generation_user_prompt("Review code", &[]);
        assert!(!plain.contains("must NOT be used"));
        let guarded = build_agent_generation_user_prompt(
            "Review code",
            &["reviewer".to_string(), "tester".to_string()],
        );
        assert!(guarded.contains("reviewer, tester"));
        assert!(guarded.ends_with("Return ONLY the JSON object, no other text."));
    }

    #[test]
    fn parser_accepts_direct_and_embedded_json_and_rejects_invalid_shapes() {
        let json = r#"{"identifier":"reviewer","whenToUse":"Use this agent when reviewing","systemPrompt":"You are a reviewer"}"#;
        assert_eq!(
            parse_generated_agent_response(json).unwrap().identifier,
            "reviewer"
        );
        assert_eq!(
            parse_generated_agent_response(&format!("```json\n{json}\n```"))
                .unwrap()
                .identifier,
            "reviewer"
        );
        assert_eq!(
            parse_generated_agent_response("nothing").unwrap_err(),
            "No JSON object found in response"
        );
        assert!(
            parse_generated_agent_response(
                r#"{"identifier":"","whenToUse":"x","systemPrompt":"y"}"#
            )
            .unwrap_err()
            .contains("Invalid")
        );
    }

    #[test]
    fn injected_backend_receives_system_and_user_prompts_without_network() {
        let generated = futures::executor::block_on(generate_agent_with_backend(
            "Run tests", &["tester".to_string()], true,
            |user, system| async move {
                assert!(user.contains("tester"));
                assert!(system.contains("Agent Memory Instructions"));
                Ok(r#"{"identifier":"test-runner","whenToUse":"Use this agent when tests are needed","systemPrompt":"You are a test runner"}"#.to_string())
            },
        )).unwrap();
        assert_eq!(generated.identifier, "test-runner");
    }

    #[test]
    fn runtime_cancellation_drops_an_in_flight_backend() {
        let runtime = AgentGeneratorRuntime::new(|_| {
            Box::pin(async {
                futures::future::pending::<()>().await;
                unreachable!()
            })
        });
        let cancellation = AgentGenerationCancellation::new();
        let cancellation_for_task = cancellation.clone();
        let result = futures::executor::block_on(async move {
            let cancel = async move {
                futures_timer::Delay::new(std::time::Duration::from_millis(10)).await;
                cancellation_for_task.cancel();
            };
            let generate = runtime.generate_with_cancellation("agent".to_string(), cancellation);
            let (_, result) = futures::join!(cancel, generate);
            result
        });
        assert_eq!(result.unwrap_err(), "Generation cancelled");
    }
}
