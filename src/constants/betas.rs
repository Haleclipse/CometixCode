//! Beta header constants.
//!
//! Maps to: CC `constants/betas.ts`.

/// Maps to CC `constants/betas.ts` `CLAUDE_CODE_20250219_BETA_HEADER`.
pub const CLAUDE_CODE_20250219_BETA_HEADER: &str = "claude-code-20250219";
/// Maps to CC `constants/betas.ts` `INTERLEAVED_THINKING_BETA_HEADER`.
pub const INTERLEAVED_THINKING_BETA_HEADER: &str = "interleaved-thinking-2025-05-14";
/// Maps to CC `constants/betas.ts` `CONTEXT_1M_BETA_HEADER`.
pub const CONTEXT_1M_BETA_HEADER: &str = "context-1m-2025-08-07";
/// Maps to CC `constants/betas.ts` `CONTEXT_MANAGEMENT_BETA_HEADER`.
pub const CONTEXT_MANAGEMENT_BETA_HEADER: &str = "context-management-2025-06-27";
/// Maps to CC `constants/betas.ts` `STRUCTURED_OUTPUTS_BETA_HEADER`.
pub const STRUCTURED_OUTPUTS_BETA_HEADER: &str = "structured-outputs-2025-12-15";
/// Maps to CC `constants/betas.ts` `WEB_SEARCH_BETA_HEADER`.
pub const WEB_SEARCH_BETA_HEADER: &str = "web-search-2025-03-05";
/// Maps to CC `constants/betas.ts` `TOOL_SEARCH_BETA_HEADER_1P`.
pub const TOOL_SEARCH_BETA_HEADER_1P: &str = "advanced-tool-use-2025-11-20";
/// Maps to CC `constants/betas.ts` `TOOL_SEARCH_BETA_HEADER_3P`.
pub const TOOL_SEARCH_BETA_HEADER_3P: &str = "tool-search-tool-2025-10-19";
/// Maps to CC `constants/betas.ts` `EFFORT_BETA_HEADER`.
pub const EFFORT_BETA_HEADER: &str = "effort-2025-11-24";
/// Maps to CC `constants/betas.ts` `TASK_BUDGETS_BETA_HEADER`.
pub const TASK_BUDGETS_BETA_HEADER: &str = "task-budgets-2026-03-13";
/// Maps to CC `constants/betas.ts` `PROMPT_CACHING_SCOPE_BETA_HEADER`.
pub const PROMPT_CACHING_SCOPE_BETA_HEADER: &str = "prompt-caching-scope-2026-01-05";
/// Maps to CC `constants/betas.ts` `REDACT_THINKING_BETA_HEADER`.
pub const REDACT_THINKING_BETA_HEADER: &str = "redact-thinking-2026-02-12";
/// Maps to CC `constants/betas.ts` `ADVISOR_BETA_HEADER`.
pub const ADVISOR_BETA_HEADER: &str = "advisor-tool-2026-03-01";
/// Maps to CC `constants/betas.ts` `CLI_INTERNAL_BETA_HEADER` value gate.
#[cfg(feature = "anthropic_internal")]
pub const CLI_INTERNAL_BETA_HEADER: &str = "cli-internal-2026-02-09";
/// External CC bundles expose the same constant as the empty string.
#[cfg(not(feature = "anthropic_internal"))]
pub const CLI_INTERNAL_BETA_HEADER: &str = "";
