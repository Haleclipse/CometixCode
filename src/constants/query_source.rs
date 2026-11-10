//! Query source constants.
//! Maps to CC `constants/querySource.ts`.

/// Prefix of CC's one PARAMETERIZED agent source,
/// `` `agent:builtin:${agentType}` `` (`utils/promptCategory.ts:23`).
pub const AGENT_BUILTIN_SOURCE_PREFIX: &str = "agent:builtin:";

/// Source of a query turn.
///
/// CC's real domain is an OPEN string, not the union `constants/querySource.ts`
/// declares. That file is a `@generated-stub` ("missing from sourcemap … Type
/// definitions inferred from codebase usage patterns") and its own producers
/// cast past it:
///
/// ```ts
/// // utils/promptCategory.ts:20-27
/// if (isBuiltInAgent) {
///   // TODO: avoid this cast
///   return agentType
///     ? (`agent:builtin:${agentType}` as QuerySource)
///     : 'agent:default'
/// } else {
///   return 'agent:custom'
/// }
/// ```
///
/// The domain is namespaced, and consumers branch on the NAMESPACE, not on the
/// whole string: `query.ts:377-378` and `services/api/claude.ts:1067`
/// `querySource.startsWith('agent:')`, `microCompact.ts:250` /
/// `postCompactCleanup.ts:38` / `log.ts:337`
/// `querySource.startsWith('repl_main_thread')`,
/// `promptCacheBreakDetection.ts:155` / `claude.ts:430`
/// `querySource.startsWith(pattern)`. Not one CC site produces the bare string
/// `'agent'` — every agent turn goes through `getQuerySourceForAgent`, so
/// `agent:builtin:<type>` / `agent:default` / `agent:custom` are the three real
/// agent sources.
///
/// This enum names the closed subset the port branches on. Variants that carry
/// a payload store the SERIALIZED CC string (not a fragment of it) so
/// [`QuerySource::as_api_source`] stays a borrow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuerySource {
    /// Maps to `utils/promptCategory.ts` `getQuerySourceForREPL()` default.
    Prompt,
    /// Maps to CC query sources that must not recursively autocompact.
    Compact,
    SessionMemory,
    MarbleOrigami,
    /// Maps to source strings present in `constants/querySource.ts`.
    Sdk,
    Bridge,
    Browser,
    Api,
    Webhook,
    Skill,
    /// The port's remaining COARSE agent stand-in, serializing to the stub
    /// union's `'agent'`. No CC site produces that string; the three variants
    /// below are what `getQuerySourceForAgent` actually returns. Callers still
    /// on this variant are ones whose CC counterpart hardcodes `'agent:custom'`
    /// (`utils/swarm/inProcessRunner.ts:1196`, `tools/SkillTool/SkillTool.ts:232`,
    /// `utils/processUserInput/processSlashCommand.tsx:227`/`:308`) and are
    /// owned by other files.
    Agent,
    /// Maps to CC `utils/promptCategory.ts:22-23`
    /// `` `agent:builtin:${agentType}` as QuerySource ``. Payload is the whole
    /// serialized source, e.g. `agent:builtin:fork`; build it with
    /// [`QuerySource::agent_builtin`].
    AgentBuiltin(String),
    /// Maps to CC `utils/promptCategory.ts:24` `'agent:default'` — a built-in
    /// agent with no `agentType` (JS truthiness, so an EMPTY type lands here
    /// too).
    AgentDefault,
    /// Maps to CC `utils/promptCategory.ts:26` `'agent:custom'`.
    AgentCustom,
    /// Maps to CC forked-agent service sources.
    AutoDream,
    ExtractMemories,
    PromptSuggestion,
    AgentSummary,
    /// Maps to CC `side_question` (`/btw` forked agent).
    SideQuestion,
    /// @nyan: prompt-cache warming fork (compaction C12). It replays the main
    /// thread's request verbatim to extend the cache TTL, so it must never
    /// change the request prefix — see `is_cache_sharing_fork`.
    CacheWarm,
    /// Maps to CC execAgentHook.ts querySource.
    HookAgent,
    Unknown,
}

/// Serialized query source string used by API request options.
pub type QuerySourceString = String;

/// Query source subset used by API retry-source gating.
///
/// Maps to CC `constants/querySource.ts` string values as consumed by
/// `services/api/withRetry.ts` foreground 529 retry logic. This lives in the
/// constants module rather than `services/api/with_retry.rs` so query-source
/// vocabulary has one Rust owner.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RetryQuerySource {
    ReplMainThread,
    Sdk,
    AgentCustom,
    AgentDefault,
    AgentBuiltin,
    Compact,
    HookAgent,
    HookPrompt,
    VerificationAgent,
    SideQuestion,
    AutoMode,
    BashClassifier,
    /// Background/non-foreground query sources (summaries, titles, etc.).
    Other(String),
}

impl QuerySource {
    /// Maps to CC `utils/promptCategory.ts:23`
    /// `` `agent:builtin:${agentType}` ``.
    pub fn agent_builtin(agent_type: &str) -> Self {
        QuerySource::AgentBuiltin(format!("{AGENT_BUILTIN_SOURCE_PREFIX}{agent_type}"))
    }

    /// The `agentType` inside an [`QuerySource::AgentBuiltin`] source.
    pub fn builtin_agent_type(&self) -> Option<&str> {
        match self {
            QuerySource::AgentBuiltin(source) => source.strip_prefix(AGENT_BUILTIN_SOURCE_PREFIX),
            _ => None,
        }
    }

    /// Maps to CC `querySource.startsWith('agent:')` (`query.ts:377`,
    /// `services/api/claude.ts:1067`) — "this turn belongs to a subagent".
    ///
    /// [`QuerySource::Agent`] is included even though `'agent'` does NOT
    /// satisfy CC's prefix test: the variant is this port's own coarse
    /// stand-in for callers whose CC counterpart emits `agent:custom`, so
    /// excluding it here would silently move those turns onto the MAIN-thread
    /// branch of every consumer below.
    pub fn is_agent(&self) -> bool {
        matches!(
            self,
            QuerySource::Agent
                | QuerySource::AgentBuiltin(_)
                | QuerySource::AgentDefault
                | QuerySource::AgentCustom
        )
    }

    /// Serialized query source used for API calls.
    ///
    /// Returns a borrow rather than `&'static str` because
    /// [`QuerySource::AgentBuiltin`] carries a runtime agent type.
    pub fn as_api_source(&self) -> &str {
        match self {
            QuerySource::Prompt => crate::utils::prompt_category::get_query_source_for_repl(self),
            QuerySource::Compact => "compact",
            QuerySource::SessionMemory => "session_memory",
            QuerySource::MarbleOrigami => "marble_origami",
            QuerySource::Sdk => "sdk",
            QuerySource::Bridge => "bridge",
            QuerySource::Browser => "browser",
            QuerySource::Api => "api",
            QuerySource::Webhook => "webhook",
            QuerySource::Skill => "skill",
            QuerySource::Agent => "agent",
            QuerySource::AgentBuiltin(source) => source,
            QuerySource::AgentDefault => "agent:default",
            QuerySource::AgentCustom => "agent:custom",
            QuerySource::AutoDream => "auto_dream",
            QuerySource::ExtractMemories => "extract_memories",
            QuerySource::PromptSuggestion => "prompt_suggestion",
            QuerySource::AgentSummary => "agent_summary",
            QuerySource::SideQuestion => "side_question",
            // @nyan: nyan-only source; no CC string to mirror.
            QuerySource::CacheWarm => "cache_warm",
            QuerySource::HookAgent => "hook_agent",
            QuerySource::Unknown => "unknown",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_query_source_matches_official_repl_default() {
        assert_eq!(QuerySource::Prompt.as_api_source(), "repl_main_thread");
    }

    #[test]
    fn compact_query_sources_match_official_recursion_guard_strings() {
        assert_eq!(QuerySource::Compact.as_api_source(), "compact");
        assert_eq!(QuerySource::SessionMemory.as_api_source(), "session_memory");
        assert_eq!(QuerySource::MarbleOrigami.as_api_source(), "marble_origami");
    }

    #[test]
    fn forked_agent_query_sources_match_official_service_strings() {
        assert_eq!(QuerySource::AutoDream.as_api_source(), "auto_dream");
        assert_eq!(
            QuerySource::ExtractMemories.as_api_source(),
            "extract_memories"
        );
        assert_eq!(
            QuerySource::PromptSuggestion.as_api_source(),
            "prompt_suggestion"
        );
        assert_eq!(QuerySource::AgentSummary.as_api_source(), "agent_summary");
    }

    #[test]
    fn cache_warm_source_serializes_for_the_warming_fork() {
        assert_eq!(QuerySource::CacheWarm.as_api_source(), "cache_warm");
        assert!(!QuerySource::CacheWarm.is_agent());
    }

    /// CC `utils/promptCategory.ts:20-27` — the three strings
    /// `getQuerySourceForAgent` can return, serialized exactly as CC writes
    /// them. `agent:builtin:` is a PREFIX of a longer string, which is why
    /// `services/api/claude.ts:1067`'s `startsWith('agent:')` and
    /// `withRetry.ts:62-82`'s exact-set membership disagree about it in CC.
    #[test]
    fn agent_query_sources_serialize_to_the_official_colon_namespace() {
        assert_eq!(
            QuerySource::agent_builtin("fork").as_api_source(),
            "agent:builtin:fork"
        );
        assert_eq!(
            QuerySource::agent_builtin("general-purpose").as_api_source(),
            "agent:builtin:general-purpose"
        );
        assert_eq!(QuerySource::AgentDefault.as_api_source(), "agent:default");
        assert_eq!(QuerySource::AgentCustom.as_api_source(), "agent:custom");

        assert_eq!(
            QuerySource::agent_builtin("fork").builtin_agent_type(),
            Some("fork")
        );
        assert_eq!(QuerySource::AgentCustom.builtin_agent_type(), None);
    }

    /// `query.ts:377` / `claude.ts:1067` `querySource.startsWith('agent:')`:
    /// every agent source answers yes, and no main-thread or service source
    /// does. Widening the enum without widening this predicate would move
    /// every subagent turn onto the main-thread branch of `query.rs:540`
    /// (diagnostic reset), `:2882` (`agent_id` on the tool context), `:3818`
    /// (tool-use summaries) and `attachments.rs:1890`/`:2587` (diagnostics).
    #[test]
    fn is_agent_covers_every_agent_source_and_nothing_else() {
        for source in [
            QuerySource::Agent,
            QuerySource::agent_builtin("fork"),
            QuerySource::AgentDefault,
            QuerySource::AgentCustom,
        ] {
            assert!(source.is_agent(), "{source:?} is an agent source");
        }
        for source in [
            QuerySource::Prompt,
            QuerySource::Sdk,
            QuerySource::Compact,
            QuerySource::SessionMemory,
            // `agent_summary` shares a prefix with `agent:` only up to the
            // colon CC's test depends on — `'agent_summary'.startsWith('agent:')`
            // is false.
            QuerySource::AgentSummary,
            QuerySource::Unknown,
        ] {
            assert!(!source.is_agent(), "{source:?} is not an agent source");
        }
    }
}
