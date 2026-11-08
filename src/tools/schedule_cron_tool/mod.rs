//! Incremental port of official `ScheduleCronTool/*`.
//! Schema/prompt metadata is API-visible. Session-only create/delete/list +
//! scheduler enable are live; durable `.nyan/scheduled_tasks.json` writes
//! stay gated behind DurableCron.

pub mod prompt;
pub mod ui;

const MAX_JOBS: usize = 50;

/// Maps to: CC `CronCreateTool.ts:27-42` `inputSchema`.
pub fn cron_create_input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::semantic_boolean::semantic_boolean;
        use crate::utils::zod as zod;
        zod::strict_object(vec![
            (
                "cron",
                zod::string().describe(
                    "Standard 5-field cron expression in local time: \"M H DoM Mon DoW\" (e.g. \"*/5 * * * *\" = every 5 minutes, \"30 14 28 2 *\" = Feb 28 at 2:30pm local once).",
                ),
            ),
            (
                "prompt",
                zod::string().describe("The prompt to enqueue at each fire time."),
            ),
            (
                "recurring",
                semantic_boolean(zod::boolean().optional()).describe(format!(
                    "true (default) = fire on every cron match until deleted or auto-expired after {} days. false = fire once at the next match, then auto-delete. Use false for \"remind me at X\" one-shot requests with pinned minute/hour/dom/month.",
                    prompt::DEFAULT_MAX_AGE_DAYS
                )),
            ),
            (
                "durable",
                semantic_boolean(zod::boolean().optional()).describe(
                    "true = persist to .nyan/scheduled_tasks.json and survive restarts. false (default) = in-memory only, dies when this Claude session ends. Use true only when the user asks the task to survive across sessions.",
                ),
            ),
        ])
    })
}

/// Maps to: CC `CronDeleteTool.ts:20-24` `inputSchema`.
pub fn cron_delete_input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod;
        zod::strict_object(vec![(
            "id",
            zod::string().describe("Job ID returned by CronCreate."),
        )])
    })
}

/// Maps to: CC `CronListTool.ts:17` `inputSchema`.
pub fn cron_list_input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| crate::utils::zod::strict_object(Vec::new()))
}

pub fn cron_create_tool_schema() -> crate::types::tools::Tool {
    let durable_enabled = prompt::is_durable_cron_enabled();
    crate::types::tools::Tool {
        name: prompt::CRON_CREATE_TOOL_NAME.to_string(),
        description: prompt::build_cron_create_prompt(durable_enabled),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(
            cron_create_input_schema(),
        ),
        ..Default::default()
    }
}

pub fn cron_delete_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: prompt::CRON_DELETE_TOOL_NAME.to_string(),
        description: prompt::build_cron_delete_prompt(prompt::is_durable_cron_enabled()),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(
            cron_delete_input_schema(),
        ),
        ..Default::default()
    }
}

pub fn cron_list_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: prompt::CRON_LIST_TOOL_NAME.to_string(),
        description: prompt::build_cron_list_prompt(prompt::is_durable_cron_enabled()),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(cron_list_input_schema()),
        ..Default::default()
    }
}

/// Maps to: CC `tools/ScheduleCronTool/CronCreateTool.ts:54` `export type
/// CreateOutput = z.infer<OutputSchema>` (schema at :45-52) — the tool's
/// yielded/persisted `toolUseResult` shape; the render path recovers it via
/// `outputSchema.safeParse` ([`ui::parse_create_output`] is the stand-in).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateOutput {
    pub id: String,
    pub human_schedule: String,
    pub recurring: bool,
    /// CC `durable` is optional in the schema.
    pub durable: Option<bool>,
}

/// Maps to: CC `tools/ScheduleCronTool/CronDeleteTool.ts:33` `export type
/// DeleteOutput` (schema at :27-31).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeleteOutput {
    pub id: String,
}

/// CC `CronListTool.ts:22-30` inline job object (anonymous `z.object`, so the
/// name is ours).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListJob {
    pub id: String,
    pub cron: String,
    pub human_schedule: String,
    pub prompt: String,
    /// CC `recurring` / `durable` are optional in the schema.
    pub recurring: Option<bool>,
    pub durable: Option<bool>,
}

/// Maps to: CC `tools/ScheduleCronTool/CronListTool.ts:35` `export type
/// ListOutput` (schema at :20-33).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListOutput {
    pub jobs: Vec<ListJob>,
}

/// Maps to: CC `getTeammateContext()?.agentId`
/// (`utils/teammateContext.ts:47-49`). Rust has no `AsyncLocalStorage`; the
/// dynamic team context is the ported carrier of teammate identity
/// (`utils/teammate.rs:61`).
fn teammate_agent_id() -> Option<String> {
    crate::utils::teammate::get_agent_id()
}

fn cron_input_string(input: &serde_json::Value, field: &str) -> String {
    input
        .get(field)
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Maps to: CC `tools/ScheduleCronTool/CronCreateTool.ts:82-116` `validateInput`.
pub(crate) fn cron_create_validate(input: &serde_json::Value) -> crate::tool::ValidationResult {
    use crate::utils::cron::parse_cron_expression;
    use crate::utils::cron_tasks::{list_all_cron_tasks, next_cron_run_ms};
    use crate::utils::semantic_boolean::parse_json_bool;

    let cron = cron_input_string(input, "cron");
    if parse_cron_expression(&cron).is_none() {
        return crate::tool::ValidationResult::error(
            format!("Invalid cron expression '{cron}'. Expected 5 fields: M H DoM Mon DoW."),
            1,
        );
    }
    if next_cron_run_ms(&cron, now_ms()).is_none() {
        return crate::tool::ValidationResult::error(
            format!("Cron expression '{cron}' does not match any calendar date in the next year."),
            2,
        );
    }
    if list_all_cron_tasks().len() >= MAX_JOBS {
        return crate::tool::ValidationResult::error(
            format!("Too many scheduled jobs (max {MAX_JOBS}). Cancel one first."),
            3,
        );
    }
    // Teammates don't persist across sessions, so a durable teammate cron
    // would orphan on restart (agentId would point to a nonexistent teammate).
    let durable_requested = input
        .get("durable")
        .and_then(parse_json_bool)
        .unwrap_or(false);
    if durable_requested && teammate_agent_id().is_some() {
        return crate::tool::ValidationResult::error(
            "durable crons are not supported for teammates (teammates do not persist across sessions)",
            4,
        );
    }
    crate::tool::ValidationResult::Ok
}

/// Schedule a cron job.
/// Maps to: CC `tools/ScheduleCronTool/CronCreateTool.ts:117-142` `call`.
pub(crate) fn cron_create_output(input: &serde_json::Value) -> Result<CreateOutput, String> {
    use crate::utils::cron::cron_to_human;
    use crate::utils::cron_tasks::{add_cron_task, set_scheduled_tasks_enabled};
    use crate::utils::semantic_boolean::parse_json_bool;

    let cron = cron_input_string(input, "cron");
    let prompt_value = cron_input_string(input, "prompt");
    let recurring = input
        .get("recurring")
        .and_then(parse_json_bool)
        .unwrap_or(true);
    let durable_requested = input
        .get("durable")
        .and_then(parse_json_bool)
        .unwrap_or(false);
    // Kill switch forces session-only; the schema stays stable so the model
    // sees no validation errors when the gate flips mid-session.
    let durable = durable_requested && prompt::is_durable_cron_enabled();
    let human_schedule = cron_to_human(&cron, false);
    let id = add_cron_task(cron, prompt_value, recurring, durable, teammate_agent_id())?;
    // Enable the scheduler so the task fires in this session.
    set_scheduled_tasks_enabled(true);

    Ok(CreateOutput {
        id,
        human_schedule,
        recurring,
        durable: Some(durable),
    })
}

pub(crate) fn cron_create_model_content(output: &CreateOutput) -> String {
    // JS truthiness: an absent `durable` reads falsy.
    let where_text = if output.durable.unwrap_or(false) {
        "Persisted to .nyan/scheduled_tasks.json"
    } else {
        "Session-only (not written to disk, dies when Claude exits)"
    };
    if output.recurring {
        format!(
            "Scheduled recurring job {} ({}). {where_text}. Auto-expires after {} days. Use CronDelete to cancel sooner.",
            output.id,
            output.human_schedule,
            prompt::DEFAULT_MAX_AGE_DAYS
        )
    } else {
        format!(
            "Scheduled one-shot task {} ({}). {where_text}. It will fire once then auto-delete.",
            output.id, output.human_schedule
        )
    }
}

/// Maps to: CC `tools/ScheduleCronTool/CronDeleteTool.ts:61-81` `validateInput`.
pub(crate) fn cron_delete_validate(input: &serde_json::Value) -> crate::tool::ValidationResult {
    use crate::utils::cron_tasks::list_all_cron_tasks;

    let id = cron_input_string(input, "id");
    let tasks = list_all_cron_tasks();
    let Some(task) = tasks.iter().find(|task| task.id == id) else {
        return crate::tool::ValidationResult::error(format!("No scheduled job with id '{id}'"), 1);
    };
    // Teammates may only delete their own crons.
    if let Some(agent_id) = teammate_agent_id()
        && task.agent_id.as_deref() != Some(agent_id.as_str())
    {
        return crate::tool::ValidationResult::error(
            format!("Cannot delete cron job '{id}': owned by another agent"),
            2,
        );
    }
    crate::tool::ValidationResult::Ok
}

/// Cancel a scheduled cron job.
/// Maps to: CC `tools/ScheduleCronTool/CronDeleteTool.ts` `call` (:82) /
/// `utils/cronTasks.ts` `removeCronTasks` (:231).
pub(crate) fn cron_delete_output(input: &serde_json::Value) -> Result<DeleteOutput, String> {
    use crate::utils::cron_tasks::remove_cron_tasks;

    let id = cron_input_string(input, "id");
    remove_cron_tasks(std::slice::from_ref(&id))?;
    Ok(DeleteOutput { id })
}

pub(crate) fn cron_delete_model_content(output: &DeleteOutput) -> String {
    format!("Cancelled job {}.", output.id)
}

/// List scheduled cron jobs.
/// Maps to: CC `tools/ScheduleCronTool/CronListTool.ts` `call` (:63) /
/// `utils/cronTasks.ts` `listAllCronTasks` (:288).
pub(crate) fn cron_list_output() -> ListOutput {
    use crate::utils::cron::cron_to_human;
    use crate::utils::cron_tasks::list_all_cron_tasks;

    // Teammates only see their own crons; team lead (no ctx) sees all.
    let agent_id = teammate_agent_id();
    let jobs = list_all_cron_tasks()
        .into_iter()
        .filter(|task| match agent_id.as_deref() {
            Some(agent_id) => task.agent_id.as_deref() == Some(agent_id),
            None => true,
        })
        .map(|task| ListJob {
            id: task.id,
            human_schedule: cron_to_human(&task.cron, false),
            cron: task.cron,
            prompt: task.prompt,
            // CC :75-76 builds the job with conditional keys:
            // `...(t.recurring ? { recurring: true } : {})` — the key exists
            // only when truthy; `...(t.durable === false ? { durable: false }
            // : {})` — only an explicit false is surfaced.
            recurring: task.recurring.then_some(true),
            durable: (task.durable == Some(false)).then_some(false),
        })
        .collect::<Vec<_>>();
    ListOutput { jobs }
}

pub(crate) fn cron_list_model_content(output: &ListOutput) -> String {
    if output.jobs.is_empty() {
        return "No scheduled jobs.".to_string();
    }
    output
        .jobs
        .iter()
        .map(|job| {
            format!(
                "{} — {}{}{}: {}",
                job.id,
                job.human_schedule,
                // CC map :89: `j.recurring ?` — truthy; the projection above
                // only ever yields Some(true) or None.
                if job.recurring.unwrap_or(false) {
                    " (recurring)"
                } else {
                    " (one-shot)"
                },
                // CC map :89: `j.durable === false` — only the explicit false.
                if job.durable == Some(false) {
                    " [session-only]"
                } else {
                    ""
                },
                crate::utils::truncate::truncate(&job.prompt, 80, true)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn cron_error_result(error: String) -> crate::tool::ToolResult {
    crate::tool::ToolResult {
        data: crate::tool::ToolOutput::Composed {
            content: format!("<tool_use_error>{error}</tool_use_error>"),
            status: crate::types::message::ToolResultStatus::Error,
        },
        new_messages: Vec::new(),
    }
}

/// Behavioral half of CC `CronCreateTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct CronCreateTool;

impl crate::tool::ToolCall for CronCreateTool {
    fn name(&self) -> &'static str {
        "CronCreate"
    }

    /// Maps to: CC `CronCreateTool.ts:76-78` `async prompt() { return
    /// buildCronCreatePrompt(isDurableCronEnabled()) }`.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::build_cron_create_prompt(prompt::is_durable_cron_enabled())
    }

    /// Maps to: CC `ScheduleCronTool/CronCreateTool.ts:27-42`.
    fn normalize_input(&self, args: &serde_json::Value) -> serde_json::Value {
        let mut parsed = args.clone();
        for field in ["recurring", "durable"] {
            crate::utils::semantic_boolean::preprocess_object_field(&mut parsed, field);
        }
        parsed
    }

    /// Maps to: CC `CronCreateTool.ts:67-69` `isEnabled()`.
    fn is_enabled(&self) -> bool {
        prompt::is_kairos_cron_enabled()
    }

    /// Maps to: CC `CronCreateTool.searchHint` (:58).
    fn search_hint(&self) -> Option<&'static str> {
        Some("schedule a recurring or one-shot prompt")
    }

    /// Maps to: CC `CronCreateTool.ts:73-75` `description()` — durable-gated
    /// two-state copy.
    fn description(&self, _args: &serde_json::Value) -> String {
        prompt::build_cron_create_description(prompt::is_durable_cron_enabled())
    }

    /// Maps to: CC `CronCreateTool.ts:79-81` `getPath()`.
    fn get_path(&self, _args: &serde_json::Value) -> Option<String> {
        Some(
            crate::utils::cron_tasks::get_cron_file_path()
                .display()
                .to_string(),
        )
    }

    /// Maps to: CC `CronCreateTool.shouldDefer` (:60).
    fn should_defer(&self) -> bool {
        true
    }

    fn max_result_size_chars(&self) -> usize {
        100_000
    }

    /// Maps to: CC `CronCreateTool.toAutoClassifierInput` (:70-72).
    fn to_auto_classifier_input(&self, args: &serde_json::Value) -> String {
        format!(
            "{}: {}",
            cron_input_string(args, "cron"),
            cron_input_string(args, "prompt")
        )
    }

    fn validate_input(
        &self,
        args: &serde_json::Value,
        _context: &crate::tool::ToolUseContext,
    ) -> crate::tool::ValidationResult {
        cron_create_validate(&self.normalize_input(args))
    }

    fn call<'a>(
        &'a self,
        args: &'a serde_json::Value,
        _request: &'a crate::types::permissions::PermissionRequest,
        _context: &'a crate::tool::ToolUseContext,
        _can_use_tool: Option<crate::tool::CanUseToolFn<'a>>,
        _parent_message: Option<&'a crate::types::message::AssistantMessage>,
        _on_progress: Option<crate::tool::ToolCallProgressFn<'a>>,
    ) -> futures::future::BoxFuture<'a, crate::tool::ToolResult> {
        Box::pin(async move {
            match cron_create_output(args) {
                Ok(output) => crate::tool::ToolResult {
                    data: crate::tool::ToolOutput::CronCreate(output),
                    new_messages: Vec::new(),
                },
                Err(error) => cron_error_result(error),
            }
        })
    }

    /// Maps to: CC `tools/ScheduleCronTool/CronCreateTool.ts`
    /// `mapToolResultToToolResultBlockParam` (:143-154).
    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        match data {
            crate::tool::ToolOutput::CronCreate(output) => (
                cron_create_model_content(output),
                crate::types::message::ToolResultStatus::Success,
            ),
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (
                "<tool_use_error>CronCreate returned an unexpected output variant</tool_use_error>"
                    .to_string(),
                crate::types::message::ToolResultStatus::Error,
            ),
        }
    }

    /// Maps to: CC recording CronCreateTool's `CreateOutput` as the message's
    /// `toolUseResult`.
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::CronCreate(output) => {
                Some(crate::tools::schedule_cron_tool::ui::create_output_to_value(output))
            }
            crate::tool::ToolOutput::Composed {
                content,
                status: crate::types::message::ToolResultStatus::Error,
                ..
            } => {
                let message = crate::utils::messages::extract_tag(content, "tool_use_error")
                    .unwrap_or_else(|| content.clone());
                Some(serde_json::Value::String(message))
            }
            _ => None,
        }
    }
}

/// Behavioral half of CC `CronDeleteTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct CronDeleteTool;

impl crate::tool::ToolCall for CronDeleteTool {
    fn name(&self) -> &'static str {
        "CronDelete"
    }

    /// Maps to: CC `CronDeleteTool.ts:55-57` `async prompt() { return
    /// buildCronDeletePrompt(isDurableCronEnabled()) }`.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::build_cron_delete_prompt(prompt::is_durable_cron_enabled())
    }

    /// Maps to: CC `CronDeleteTool.ts:46-48` `isEnabled()`.
    fn is_enabled(&self) -> bool {
        prompt::is_kairos_cron_enabled()
    }

    /// Maps to: CC `CronDeleteTool.searchHint` (:37).
    fn search_hint(&self) -> Option<&'static str> {
        Some("cancel a scheduled cron job")
    }

    /// Maps to: CC `CronDeleteTool.ts:52-54` `description()`.
    fn description(&self, _args: &serde_json::Value) -> String {
        prompt::CRON_DELETE_DESCRIPTION.to_string()
    }

    /// Maps to: CC `CronDeleteTool.ts:58-60` `getPath()`.
    fn get_path(&self, _args: &serde_json::Value) -> Option<String> {
        Some(
            crate::utils::cron_tasks::get_cron_file_path()
                .display()
                .to_string(),
        )
    }

    /// Maps to: CC `CronDeleteTool.shouldDefer` (:39).
    fn should_defer(&self) -> bool {
        true
    }

    fn max_result_size_chars(&self) -> usize {
        100_000
    }

    /// Maps to: CC `CronDeleteTool.toAutoClassifierInput` (:49-51).
    fn to_auto_classifier_input(&self, args: &serde_json::Value) -> String {
        cron_input_string(args, "id")
    }

    fn validate_input(
        &self,
        args: &serde_json::Value,
        _context: &crate::tool::ToolUseContext,
    ) -> crate::tool::ValidationResult {
        cron_delete_validate(args)
    }

    fn call<'a>(
        &'a self,
        args: &'a serde_json::Value,
        _request: &'a crate::types::permissions::PermissionRequest,
        _context: &'a crate::tool::ToolUseContext,
        _can_use_tool: Option<crate::tool::CanUseToolFn<'a>>,
        _parent_message: Option<&'a crate::types::message::AssistantMessage>,
        _on_progress: Option<crate::tool::ToolCallProgressFn<'a>>,
    ) -> futures::future::BoxFuture<'a, crate::tool::ToolResult> {
        Box::pin(async move {
            match cron_delete_output(args) {
                Ok(output) => crate::tool::ToolResult {
                    data: crate::tool::ToolOutput::CronDelete(output),
                    new_messages: Vec::new(),
                },
                Err(error) => cron_error_result(error),
            }
        })
    }

    /// Maps to: CC `tools/ScheduleCronTool/CronDeleteTool.ts`
    /// `mapToolResultToToolResultBlockParam` (:86-92).
    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        match data {
            crate::tool::ToolOutput::CronDelete(output) => (
                cron_delete_model_content(output),
                crate::types::message::ToolResultStatus::Success,
            ),
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (
                "<tool_use_error>CronDelete returned an unexpected output variant</tool_use_error>"
                    .to_string(),
                crate::types::message::ToolResultStatus::Error,
            ),
        }
    }

    /// Maps to: CC recording CronDeleteTool's `DeleteOutput` as the message's
    /// `toolUseResult`.
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::CronDelete(output) => {
                Some(crate::tools::schedule_cron_tool::ui::delete_output_to_value(output))
            }
            crate::tool::ToolOutput::Composed {
                content,
                status: crate::types::message::ToolResultStatus::Error,
                ..
            } => {
                let message = crate::utils::messages::extract_tag(content, "tool_use_error")
                    .unwrap_or_else(|| content.clone());
                Some(serde_json::Value::String(message))
            }
            _ => None,
        }
    }
}

/// Behavioral half of CC `CronListTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct CronListTool;

impl crate::tool::ToolCall for CronListTool {
    fn name(&self) -> &'static str {
        "CronList"
    }

    /// Maps to: CC `CronListTool.ts:60-62` `async prompt() { return
    /// buildCronListPrompt(isDurableCronEnabled()) }`.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::build_cron_list_prompt(prompt::is_durable_cron_enabled())
    }

    /// Maps to: CC `CronListTool.ts:48-50` `isEnabled()`.
    fn is_enabled(&self) -> bool {
        prompt::is_kairos_cron_enabled()
    }

    /// Maps to: CC `CronListTool.ts:57-59` `description()`.
    fn description(&self, _args: &serde_json::Value) -> String {
        prompt::CRON_LIST_DESCRIPTION.to_string()
    }

    /// Maps to: CC `CronListTool.isConcurrencySafe()` (:51-53).
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }

    /// Maps to: CC `CronListTool.isReadOnly()` (:54-56).
    fn is_read_only(&self, _args: &serde_json::Value) -> bool {
        true
    }

    /// Maps to: CC `CronListTool.searchHint` (:39).
    fn search_hint(&self) -> Option<&'static str> {
        Some("list active cron jobs")
    }

    /// Maps to: CC `CronListTool.shouldDefer` (:41).
    fn should_defer(&self) -> bool {
        true
    }

    fn max_result_size_chars(&self) -> usize {
        100_000
    }

    fn call<'a>(
        &'a self,
        args: &'a serde_json::Value,
        _request: &'a crate::types::permissions::PermissionRequest,
        _context: &'a crate::tool::ToolUseContext,
        _can_use_tool: Option<crate::tool::CanUseToolFn<'a>>,
        _parent_message: Option<&'a crate::types::message::AssistantMessage>,
        _on_progress: Option<crate::tool::ToolCallProgressFn<'a>>,
    ) -> futures::future::BoxFuture<'a, crate::tool::ToolResult> {
        Box::pin(async move {
            let _ = args;
            crate::tool::ToolResult {
                data: crate::tool::ToolOutput::CronList(cron_list_output()),
                new_messages: Vec::new(),
            }
        })
    }

    /// Maps to: CC `tools/ScheduleCronTool/CronListTool.ts`
    /// `mapToolResultToToolResultBlockParam` (:80-94).
    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        match data {
            crate::tool::ToolOutput::CronList(output) => (
                cron_list_model_content(output),
                crate::types::message::ToolResultStatus::Success,
            ),
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (
                "<tool_use_error>CronList returned an unexpected output variant</tool_use_error>"
                    .to_string(),
                crate::types::message::ToolResultStatus::Error,
            ),
        }
    }

    /// Maps to: CC recording CronListTool's `ListOutput` as the message's
    /// `toolUseResult`.
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::CronList(output) => Some(
                crate::tools::schedule_cron_tool::ui::list_output_to_value(output),
            ),
            crate::tool::ToolOutput::Composed {
                content,
                status: crate::types::message::ToolResultStatus::Error,
                ..
            } => {
                let message = crate::utils::messages::extract_tag(content, "tool_use_error")
                    .unwrap_or_else(|| content.clone());
                Some(serde_json::Value::String(message))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn cron_create_semantic_boolean_preprocessing_matches_official_schema() {
        let parsed = crate::tool::ToolCall::normalize_input(
            &super::CronCreateTool,
            &serde_json::json!({
                "cron": "* * * * *",
                "prompt": "check",
                "recurring": "true",
                "durable": "false"
            }),
        );
        assert_eq!(parsed["recurring"], serde_json::json!(true));
        assert_eq!(parsed["durable"], serde_json::json!(false));

        let invalid = crate::tool::ToolCall::normalize_input(
            &super::CronCreateTool,
            &serde_json::json!({
                "cron": "* * * * *",
                "prompt": "check",
                "recurring": "yes",
                "durable": " TRUE "
            }),
        );
        assert_eq!(invalid["recurring"], serde_json::json!("yes"));
        assert_eq!(invalid["durable"], serde_json::json!(" TRUE "));
    }

    #[test]
    fn cron_list_projection_uses_conditional_keys_like_official_spreads() {
        // CC CronListTool.ts:75-76: `...(t.recurring ? { recurring: true } :
        // {})` and `...(t.durable === false ? { durable: false } : {})`.
        let jobs = vec![
            super::ListJob {
                id: "a".to_string(),
                cron: "* * * * *".to_string(),
                human_schedule: "every minute".to_string(),
                prompt: "check".to_string(),
                recurring: Some(true),
                durable: Some(false),
            },
            super::ListJob {
                id: "b".to_string(),
                cron: "* * * * *".to_string(),
                human_schedule: "every minute".to_string(),
                prompt: "check".to_string(),
                recurring: None,
                durable: None,
            },
        ];
        let value =
            super::super::schedule_cron_tool::ui::list_output_to_value(&super::ListOutput { jobs });
        let first = &value["jobs"][0];
        assert_eq!(first.get("recurring"), Some(&serde_json::json!(true)));
        assert_eq!(first.get("durable"), Some(&serde_json::json!(false)));
        let second = &value["jobs"][1];
        assert!(second.get("recurring").is_none());
        assert!(second.get("durable").is_none());

        let content = super::cron_list_model_content(&super::ListOutput {
            jobs: vec![
                super::ListJob {
                    id: "a".to_string(),
                    cron: "* * * * *".to_string(),
                    human_schedule: "every minute".to_string(),
                    prompt: "check".to_string(),
                    recurring: Some(true),
                    durable: Some(false),
                },
                super::ListJob {
                    id: "b".to_string(),
                    cron: "* * * * *".to_string(),
                    human_schedule: "every minute".to_string(),
                    prompt: "check".to_string(),
                    recurring: None,
                    durable: None,
                },
            ],
        });
        assert!(content.contains("a — every minute (recurring) [session-only]: check"));
        assert!(content.contains("b — every minute (one-shot): check"));
    }

    #[test]
    fn cron_tool_schemas_match_official_names_and_inputs() {
        let create = super::cron_create_tool_schema();
        assert_eq!(create.name, "CronCreate");
        assert_eq!(
            create.input_schema.get("required"),
            Some(&serde_json::json!(["cron", "prompt"]))
        );
        assert!(
            create
                .input_schema
                .pointer("/properties/recurring/description")
                .and_then(|value| value.as_str())
                .is_some_and(|description| description.contains("auto-expired"))
        );

        let delete = super::cron_delete_tool_schema();
        assert_eq!(delete.name, "CronDelete");
        assert_eq!(
            delete.input_schema.get("required"),
            Some(&serde_json::json!(["id"]))
        );

        let list = super::cron_list_tool_schema();
        assert_eq!(list.name, "CronList");
        // CC `CronListTool.ts:17` is `z.strictObject({})`, which zod projects
        // without a `required` key.
        assert_eq!(list.input_schema.get("required"), None);
    }
}
