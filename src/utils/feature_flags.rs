//! Hardcoded feature-switch collection for CC GrowthBook/Statsig-controlled
//! gates and build features.
//!
//! Maps to CC GrowthBook feature reads such as
//! `getFeatureValue_CACHED_MAY_BE_STALE(...)` and
//! `checkStatsigFeatureGate_CACHED_MAY_BE_STALE(...)`. @nyan: there is no
//! GrowthBook or Statsig client, cache, or override layer; this
//! source-controlled table is the only place these gates are decided.

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FeatureFlag {
    /// Maps to CC GrowthBook `tengu_immediate_model_command`.
    ImmediateModelCommand,
    /// Maps to CC GrowthBook `tengu_keybinding_customization_release`.
    KeybindingCustomization,
    /// Maps to CC GrowthBook external-user killswitch `tengu_amber_flint`.
    AgentSwarmsExternalKillswitch,
    /// Maps to CC GrowthBook `tengu_amber_stoat` (Explore/Plan built-in
    /// agents A/B gate; fallback true).
    BuiltinExplorePlanAgents,
    /// Maps to CC GrowthBook channels runtime gate `tengu_harbor`.
    ChannelsEnabled,
    /// Maps to CC GrowthBook channel permission-relay gate `tengu_harbor_permissions`.
    ChannelPermissionRelay,
    /// Maps to CC GrowthBook `tengu_kairos_cron`.
    KairosCron,
    /// Maps to CC GrowthBook `tengu_kairos_cron_durable`.
    DurableCron,
    /// Maps to CC GrowthBook `tengu_surreal_dali`.
    RemoteTrigger,
    /// Maps to CC GrowthBook `tengu_glacier_2xr`.
    DeferredToolsDelta,
    /// Maps to CC Statsig/GrowthBook gate `tengu_streaming_tool_execution2`.
    StreamingToolExecution,
    /// Maps to CC Statsig/GrowthBook gate `tengu_scratch`.
    Scratchpad,
    /// Maps to CC GrowthBook `tengu_moth_copse` memory skip-index gate.
    MemorySkipIndex,
    /// Maps to CC GrowthBook `tengu_coral_fern` memory search-guidance gate.
    MemorySearchPastContext,
    /// Maps to CC GrowthBook `tengu_otk_slot_v1` max-output escalation gate.
    MaxOutputTokensEscalation,
    /// Maps to CC GrowthBook `tengu_plum_vx3` WebSearch Haiku/tool-choice gate.
    WebSearchSmallFastModel,
    /// Maps to CC `feature('NEW_INIT')` build feature.
    NewInit,
    /// Maps to CC `feature('BREAK_CACHE_COMMAND')` build feature.
    BreakCacheCommand,
    /// Maps to CC `feature('HISTORY_PICKER')` build feature.
    HistoryPicker,
    /// Maps to CC `feature('FORK_SUBAGENT')` build feature.
    ForkSubagent,
    /// Maps to CC `feature('COORDINATOR_MODE')` build feature.
    CoordinatorMode,
    /// Maps to CC `feature('PROACTIVE')` build feature.
    Proactive,
    /// Maps to CC `feature('KAIROS')` build feature (distinct from KairosCron GrowthBook).
    Kairos,
    /// Maps to CC `feature('KAIROS_CHANNELS')` build feature; every CC reader
    /// pairs it as `feature('KAIROS') || feature('KAIROS_CHANNELS')`
    /// (`main.tsx:2469/:5294`, `utils/messages.ts:4669`,
    /// `AskUserQuestionTool.tsx:232`, `EnterPlanModeTool.ts:61`,
    /// `ExitPlanModeV2Tool.ts:172`).
    KairosChannels,
    /// Maps to CC `feature('KAIROS_BRIEF')` build feature; lets Brief ship
    /// independently of the rest of Kairos.
    KairosBrief,
    /// Maps to CC GrowthBook `tengu_kairos_brief`.
    KairosBriefEntitlement,
    /// Maps to CC `feature('TRANSCRIPT_CLASSIFIER')` build feature.
    TranscriptClassifier,
    /// Maps to CC `feature('BASH_CLASSIFIER')` build feature. Distinct from
    /// CC's runtime `isClassifierPermissionsEnabled()`
    /// (`utils/permissions/bashClassifier.ts:24-26`), which the external stub
    /// hardcodes to `false`: the build flag decides which code EXISTS.
    BashClassifier,
    /// Maps to CC `feature('AGENT_MEMORY_SNAPSHOT')` build feature
    /// (`loadAgentsDir.ts:348`, `main.tsx:3279`).
    AgentMemorySnapshot,
    /// Maps to CC GrowthBook `tengu_hive_evidence` (TodoWrite verification nudge).
    HiveEvidence,
    /// Maps to CC GrowthBook `tengu_destructive_command_warning`.
    DestructiveCommandWarning,
    /// Maps to CC GrowthBook `tengu_read_dedup_killswitch`.
    ReadDedupKillswitch,
    /// Maps to CC GrowthBook `tengu_compact_line_prefix_killswitch`.
    CompactLinePrefixKillswitch,
    /// Maps to CC GrowthBook `tengu_skill_dynamic_discovery`.
    DynamicSkillDiscovery,
    /// Maps to CC `feature('BRIDGE_MODE')` build feature.
    BridgeMode,
    /// Maps to CC build feature `MCP_SKILLS` (enabled in CC 2.1.88).
    McpSkills,
    /// Maps to CC build feature `EXPERIMENTAL_SKILL_SEARCH`.
    ExperimentalSkillSearch,
    /// Maps to CC `feature('UDS_INBOX')` build feature.
    UdsInbox,
    /// Maps to CC GrowthBook `tengu_satin_quoll` per-tool output-size override map.
    PersistThresholdOverrides,
    /// Maps to CC GrowthBook `tengu_auto_mode_config` JSON config.
    AutoModeConfig,
    /// Maps to CC Statsig/GrowthBook gate `tengu_tool_pear`.
    StrictToolSchemas,
    /// Maps to CC GrowthBook `tengu_fgts`.
    FineGrainedToolStreaming,
    /// Maps to CC GrowthBook `tengu_amber_wren` Read-limits override map.
    FileReadLimitsOverride,
    /// Maps to CC GrowthBook `tengu_hawthorn_steeple` tool-result content-replacement gate.
    ToolResultContentReplacement,
    /// Maps to CC GrowthBook `tengu_hawthorn_window` per-message budget override.
    PerMessageBudgetLimitOverride,
    /// Maps to CC GrowthBook `tengu_attribution_header` (fallback true).
    AttributionHeader,
    /// Maps to CC GrowthBook `tengu_quartz_lantern` remote single-file git diff gate.
    RemoteGitDiff,
    /// Maps to CC GrowthBook `tengu_chomp_inflection` prompt-suggestion cohort gate.
    PromptSuggestion,
    /// Maps to CC GrowthBook `tengu_harbor_ledger` channel allowlist payload.
    ChannelAllowlistLedger,
    /// Maps to CC GrowthBook `tengu_herring_clock` team-memory cohort gate.
    TeamMemory,
    /// Maps to CC GrowthBook `tengu_lapis_finch` plugin hint-recommendation gate.
    PluginHintRecommendation,
    /// Maps to CC GrowthBook dynamic config `tengu_desktop_upsell`.
    DesktopUpsell,
    /// Maps to CC GrowthBook dynamic config `tengu-top-of-feed-tip`.
    EmergencyTip,
    /// Maps to CC GrowthBook `tengu_iron_gate_closed` auto-mode classifier fail-closed gate.
    IronGateClosed,
    /// Maps to CC GrowthBook `tengu_amber_prism` (`utils/messages.ts:188`), the
    /// gate `withMemoryCorrectionHint` pairs with `isAutoMemoryEnabled()`.
    MemoryCorrectionHint,
    /// Maps to CC GrowthBook `tengu_workflows_enabled` (official fallback true).
    WorkflowsEnabled,
    /// Maps to CC GrowthBook `tengu_disable_keepalive_on_econnreset`
    /// (`services/api/withRetry.ts:220-223`, fallback false).
    DisableKeepaliveOnEconnreset,
    /// Maps to CC GrowthBook kill switch `tengu_slim_subagent_claudemd`
    /// (`tools/AgentTool/runAgent.ts`, fallback true).
    SlimSubagentAgentsMd,
    /// Maps to CC Statsig gate `tengu_chair_sermon`
    /// (`utils/messages.ts#mergeUserContentBlocks`, fallback false).
    UniversalToolResultMerge,
    /// Maps to CC Statsig security gate `tengu_disable_bypass_permissions_mode`
    /// (`utils/permissions/permissionSetup.ts`, fallback false).
    DisableBypassPermissionsMode,
    /// Maps to CC GrowthBook `tengu_plugin_official_mkt_git_fallback`
    /// (`utils/plugins/marketplaceManager.ts`, fallback true).
    OfficialMarketplaceGitFallback,
    /// Maps to CC GrowthBook config `tengu_sage_compass`
    /// (`utils/advisor.ts#getAdvisorConfig`, fallback `{}`).
    Advisor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeatureSwitch {
    pub flag: FeatureFlag,
    pub cc_growthbook_name: &'static str,
    pub enabled: bool,
    pub note: &'static str,
}

/// Maps to CC GrowthBook defaults, frozen as source-controlled constants.
///
/// Change this table, not runtime config/cache parsing, when Nyan needs a
/// different behavior for an upstream GrowthBook-controlled feature.
pub const FEATURE_SWITCHES: &[FeatureSwitch] = &[
    FeatureSwitch {
        flag: FeatureFlag::ImmediateModelCommand,
        cc_growthbook_name: "tengu_immediate_model_command",
        enabled: false,
        note: "/model and /effort immediate command gate.",
    },
    FeatureSwitch {
        flag: FeatureFlag::KeybindingCustomization,
        cc_growthbook_name: "tengu_keybinding_customization_release",
        enabled: true,
        note: "Local keybindings loader/template/editor path is wired; remote GrowthBook delivery is represented by this source-controlled release switch.",
    },
    FeatureSwitch {
        flag: FeatureFlag::AgentSwarmsExternalKillswitch,
        cc_growthbook_name: "tengu_amber_flint",
        enabled: true,
        note: "External agent-swarms kill switch default; env/--agent-teams opt-in is checked separately.",
    },
    FeatureSwitch {
        flag: FeatureFlag::BuiltinExplorePlanAgents,
        cc_growthbook_name: "tengu_amber_stoat",
        enabled: true,
        note: "Explore/Plan built-in agents gate; CC fallback true ('3P default: true — Bedrock/Vertex keep agents enabled'), A/B treatment false is the exception. Build feature BUILTIN_EXPLORE_PLAN_AGENTS is ON in production (build.ts:44).",
    },
    FeatureSwitch {
        flag: FeatureFlag::ChannelsEnabled,
        cc_growthbook_name: "tengu_harbor",
        enabled: false,
        note: "MCP channel notifications runtime gate; stays off until channel/MCP runtime safety is ported.",
    },
    FeatureSwitch {
        flag: FeatureFlag::ChannelPermissionRelay,
        cc_growthbook_name: "tengu_harbor_permissions",
        enabled: false,
        note: "Channel permission relay remains disabled by default; channel callback/request/response plumbing is ported behind the official GrowthBook gate.",
    },
    FeatureSwitch {
        flag: FeatureFlag::KairosCron,
        cc_growthbook_name: "tengu_kairos_cron",
        enabled: true,
        note: "Cron scheduler runtime switch; build/tool port gates may still keep cron tools unavailable.",
    },
    FeatureSwitch {
        flag: FeatureFlag::DurableCron,
        cc_growthbook_name: "tengu_kairos_cron_durable",
        enabled: true,
        note: "Official default true; durable store, polling scheduler, and lease lock are wired.",
    },
    FeatureSwitch {
        flag: FeatureFlag::RemoteTrigger,
        cc_growthbook_name: "tengu_surreal_dali",
        enabled: false,
        note: "Remote trigger API remains disabled until CCR auth/policy/network execution is ported.",
    },
    FeatureSwitch {
        flag: FeatureFlag::DeferredToolsDelta,
        cc_growthbook_name: "tengu_glacier_2xr",
        enabled: true,
        note: "The delta announcement is the only deferred-tool listing nyan emits, so the ToolSearch prompt's system-reminder wording must match it.",
    },
    FeatureSwitch {
        flag: FeatureFlag::StreamingToolExecution,
        cc_growthbook_name: "tengu_streaming_tool_execution2",
        enabled: false,
        note: "StreamingToolExecutor stays disabled by default; staged tools can start/drain during model streaming and surface interactive prompts before final drain while full CC parity remains pending.",
    },
    FeatureSwitch {
        flag: FeatureFlag::Scratchpad,
        cc_growthbook_name: "tengu_scratch",
        enabled: false,
        note: "Scratchpad prompt/filesystem helpers are ported, but the GrowthBook gate remains disabled by default like an unavailable external cohort.",
    },
    FeatureSwitch {
        flag: FeatureFlag::MemorySkipIndex,
        cc_growthbook_name: "tengu_moth_copse",
        enabled: false,
        note: "Auto-memory MEMORY.md index guidance remains enabled by default; skip-index prompt shape is wired for parity but gated off.",
    },
    FeatureSwitch {
        flag: FeatureFlag::MemorySearchPastContext,
        cc_growthbook_name: "tengu_coral_fern",
        enabled: false,
        note: "Memory/transcript search guidance is wired but disabled by default until the upstream cohort is enabled locally.",
    },
    FeatureSwitch {
        flag: FeatureFlag::MaxOutputTokensEscalation,
        cc_growthbook_name: "tengu_otk_slot_v1",
        enabled: false,
        note: "Max-output 8k-to-64k escalation gate keeps the upstream default false until the cohort is enabled locally.",
    },
    FeatureSwitch {
        flag: FeatureFlag::WebSearchSmallFastModel,
        cc_growthbook_name: "tengu_plum_vx3",
        enabled: false,
        note: "WebSearch uses the main-loop model by default; the Haiku/tool-choice experiment remains disabled unless explicitly enabled in source.",
    },
    FeatureSwitch {
        flag: FeatureFlag::NewInit,
        cc_growthbook_name: "NEW_INIT",
        enabled: true,
        note: "Build feature is present; /init still requires NYAN_NEW_INIT at runtime.",
    },
    FeatureSwitch {
        flag: FeatureFlag::BreakCacheCommand,
        cc_growthbook_name: "BREAK_CACHE_COMMAND",
        enabled: false,
        note: "Ant-only cache-breaker system-context injection remains disabled unless explicitly enabled in source.",
    },
    FeatureSwitch {
        flag: FeatureFlag::HistoryPicker,
        cc_growthbook_name: "HISTORY_PICKER",
        // Build feature, not GrowthBook: `scripts/build.ts:49` lists it under
        // "Generally available (ON in production)".
        enabled: true,
        note: "Build feature: scripts/build.ts:49. ctrl+r opens PromptInput's history picker (PromptInput.tsx:2226-2238) and useHistorySearch's inline history:search binding is inactive (useHistorySearch.ts:236-241).",
    },
    FeatureSwitch {
        flag: FeatureFlag::ForkSubagent,
        cc_growthbook_name: "FORK_SUBAGENT",
        // Build feature, not GrowthBook: `scripts/build.ts:45` lists
        // `FORK_SUBAGENT: true` under "Generally available (ON in production)",
        // so production CC evaluates `feature('FORK_SUBAGENT')` to true and so
        // does this table.
        enabled: true,
        note: "Build feature: scripts/build.ts:45 lists FORK_SUBAGENT under \"Generally available (ON in production)\". The build flag only decides which code EXISTS; forkSubagent.ts:33-38 is the runtime gate and still returns false under coordinator mode and non-interactive sessions, which is_fork_subagent_enabled mirrors. With it on, the Agent tool omits run_in_background from its schema (AgentTool.tsx:252-254), its description carries the fork sections and the fork examples (prompt.ts:78-113/:115-154), a missing subagent_type selects the synthetic fork agent instead of general-purpose (AgentTool.tsx:480-483), and forceAsync routes EVERY spawn through the async <task-notification> model (:812).",
    },
    FeatureSwitch {
        flag: FeatureFlag::CoordinatorMode,
        cc_growthbook_name: "COORDINATOR_MODE",
        enabled: true,
        note: "Build feature for coordinator system prompt / mode detection; runtime still requires NYAN_COORDINATOR_MODE.",
    },
    FeatureSwitch {
        flag: FeatureFlag::Proactive,
        cc_growthbook_name: "PROACTIVE",
        enabled: false,
        note: "proactive/index.ts is no-source in rebuild; keep off until proactive runtime is ported.",
    },
    FeatureSwitch {
        flag: FeatureFlag::Kairos,
        cc_growthbook_name: "KAIROS",
        enabled: false,
        note: "Build feature for Kairos/proactive agent-append path; distinct from tengu_kairos_cron GrowthBook.",
    },
    FeatureSwitch {
        flag: FeatureFlag::KairosChannels,
        cc_growthbook_name: "KAIROS_CHANNELS",
        enabled: false,
        note: "Build feature for the channels-only Kairos slice; CC always reads it OR-ed with KAIROS. Off until the channels runtime is ported.",
    },
    FeatureSwitch {
        flag: FeatureFlag::KairosBrief,
        cc_growthbook_name: "KAIROS_BRIEF",
        enabled: true,
        note: "Build feature that ships SendUserMessage independently of KAIROS; activation still requires the official opt-in plus entitlement.",
    },
    FeatureSwitch {
        flag: FeatureFlag::KairosBriefEntitlement,
        cc_growthbook_name: "tengu_kairos_brief",
        enabled: false,
        note: "Official default false; NYAN_BRIEF and assistant mode grant entitlement without it.",
    },
    FeatureSwitch {
        flag: FeatureFlag::TranscriptClassifier,
        cc_growthbook_name: "TRANSCRIPT_CLASSIFIER",
        // Maps to CC feature('TRANSCRIPT_CLASSIFIER'). Enable the gate so L3/L4
        // Auto Mode wiring is live; classifyYoloAction may still return
        // unavailable until L5 lands (permissions fall through to ask).
        enabled: true,
        note: "Auto-mode classifier gate; L5 API may still return unavailable.",
    },
    FeatureSwitch {
        flag: FeatureFlag::BashClassifier,
        cc_growthbook_name: "BASH_CLASSIFIER",
        // Build feature, not GrowthBook: `scripts/build.ts:73` lists
        // `BASH_CLASSIFIER: true` under "Generally available (ON in
        // production)", so production CC evaluates `feature(...)` to true.
        enabled: true,
        note: "Build gate selecting which classifier code exists (rule-hint copy in buildYoloRejectionMessage, pendingClassifierCheck plumbing). The runtime half stays disabled separately: utils/permissions/bash_classifier.rs mirrors CC's external stub isClassifierPermissionsEnabled() -> false.",
    },
    FeatureSwitch {
        flag: FeatureFlag::AgentMemorySnapshot,
        cc_growthbook_name: "AGENT_MEMORY_SNAPSHOT",
        // Build feature, not GrowthBook: `scripts/build.ts:78` lists
        // `AGENT_MEMORY_SNAPSHOT: true` under "Generally available (ON in
        // production)", so production CC evaluates `feature(...)` to true.
        enabled: true,
        note: "Project-snapshot bootstrap for user-scope agent memory runs at definition load; the prompt-update half only records pendingSnapshotUpdate because SnapshotUpdateDialog is a @generated-stub in CC 2.1.88.",
    },
    FeatureSwitch {
        flag: FeatureFlag::HiveEvidence,
        cc_growthbook_name: "tengu_hive_evidence",
        enabled: false,
        note: "TodoWrite verification-nudge gate; off until VERIFICATION_AGENT is enabled.",
    },
    FeatureSwitch {
        flag: FeatureFlag::DestructiveCommandWarning,
        cc_growthbook_name: "tengu_destructive_command_warning",
        enabled: false,
        note: "Bash permission-dialog warning gate; source default is false.",
    },
    FeatureSwitch {
        flag: FeatureFlag::ReadDedupKillswitch,
        cc_growthbook_name: "tengu_read_dedup_killswitch",
        enabled: false,
        note: "Official default false: unchanged same-range text/notebook Reads deduplicate.",
    },
    FeatureSwitch {
        flag: FeatureFlag::CompactLinePrefixKillswitch,
        cc_growthbook_name: "tengu_compact_line_prefix_killswitch",
        enabled: false,
        note: "Official default false: killswitch off keeps the compact line-number prefix format enabled for Read output and Edit prompt wording.",
    },
    FeatureSwitch {
        flag: FeatureFlag::DynamicSkillDiscovery,
        cc_growthbook_name: "tengu_skill_dynamic_discovery",
        enabled: true,
        note: "Read-time nested .nyan/skills discovery is locally available; remote cohort delivery is represented by this explicit source-controlled seam.",
    },
    FeatureSwitch {
        flag: FeatureFlag::BridgeMode,
        cc_growthbook_name: "BRIDGE_MODE",
        // CC ships BRIDGE_MODE on in production builds (scripts/build.ts:41
        // "Generally available (ON in production)").
        enabled: true,
        note: "Remote Control build gate mirrors CC production (on); pill/dialog reachability still requires the isBridgeEnabled entitlement and a replBridgeEnabled producer (absent in Nyan — no bridge transport or settings-screen toggle).",
    },
    FeatureSwitch {
        flag: FeatureFlag::McpSkills,
        cc_growthbook_name: "MCP_SKILLS",
        enabled: true,
        note: "CC 2.1.88 build fetches MCP skills from servers that support resources (`client.ts:2172-2176`, `fetchMcpSkillsForClient`) and lists `loadedFrom: 'mcp'` commands as skills. Plain MCP prompts are never skills. Nyan has no MCP skill producer (the rebuild's `mcpSkills.ts` is a stub), so the gated readers find nothing.",
    },
    FeatureSwitch {
        flag: FeatureFlag::ExperimentalSkillSearch,
        cc_growthbook_name: "EXPERIMENTAL_SKILL_SEARCH",
        enabled: false,
        note: "Build-gated skill-search subsystem is unavailable; bundled/MCP listing filtering remains wired behind the explicit seam.",
    },
    FeatureSwitch {
        flag: FeatureFlag::UdsInbox,
        cc_growthbook_name: "UDS_INBOX",
        enabled: false,
        note: "Cross-session peer addressing (uds:/bridge: targets, ListPeers, peer transports) is unported; SendMessage keeps the official bypass-immune bridge consent gate wired behind this switch so enabling the transport cannot silently skip it.",
    },
    FeatureSwitch {
        flag: FeatureFlag::PersistThresholdOverrides,
        cc_growthbook_name: "tengu_satin_quoll",
        enabled: false,
        note: "Official default is an empty override map, so the MCP output cap falls through to the hardcoded default; the switch stays off because Nyan has no source-controlled per-tool override payload.",
    },
    FeatureSwitch {
        flag: FeatureFlag::AutoModeConfig,
        cc_growthbook_name: "tengu_auto_mode_config",
        enabled: false,
        note: "Official default is an empty config object; off keeps the auto-mode allowModels override absent so model eligibility comes from the ported denylist/allowlist rules alone.",
    },
    FeatureSwitch {
        flag: FeatureFlag::StrictToolSchemas,
        cc_growthbook_name: "tengu_tool_pear",
        enabled: false,
        note: "Statsig gate with no local cohort resolves to false: tool schemas ship without `strict` and the structured-outputs beta header stays off, matching the uncached upstream default.",
    },
    FeatureSwitch {
        flag: FeatureFlag::FineGrainedToolStreaming,
        cc_growthbook_name: "tengu_fgts",
        enabled: false,
        note: "Official default false; eager_input_streaming still opts in through NYAN_ENABLE_FINE_GRAINED_TOOL_STREAMING on first-party Anthropic base URLs.",
    },
    FeatureSwitch {
        flag: FeatureFlag::FileReadLimitsOverride,
        cc_growthbook_name: "tengu_amber_wren",
        enabled: false,
        note: "Official default is an empty override map, so Read keeps MAX_OUTPUT_SIZE / DEFAULT_MAX_OUTPUT_TOKENS; NYAN_FILE_READ_MAX_OUTPUT_TOKENS still overrides the token cap.",
    },
    FeatureSwitch {
        flag: FeatureFlag::ToolResultContentReplacement,
        cc_growthbook_name: "tengu_hawthorn_steeple",
        // @nyan: content replacement is the durable carrier for compaction
        // masking, so the fork enables it by default.
        enabled: true,
        note: "nyan default: replacement decisions are frozen and persisted so compaction can mask old tool results without changing transcript bytes.",
    },
    FeatureSwitch {
        flag: FeatureFlag::PerMessageBudgetLimitOverride,
        cc_growthbook_name: "tengu_hawthorn_window",
        enabled: false,
        note: "Official default is null, so the per-message aggregate budget stays at MAX_TOOL_RESULTS_PER_MESSAGE_CHARS; the switch stays off because Nyan has no source-controlled window payload.",
    },
    FeatureSwitch {
        flag: FeatureFlag::AttributionHeader,
        cc_growthbook_name: "tengu_attribution_header",
        enabled: true,
        note: "Official fallback is true (constants/system.ts:56 getFeatureValue_CACHED_MAY_BE_STALE('tengu_attribution_header', true)). Production still stays off because NYAN_ATTRIBUTION_HEADER defaults to 0; flip this switch to kill the header even when the env is enabled.",
    },
    FeatureSwitch {
        flag: FeatureFlag::RemoteGitDiff,
        cc_growthbook_name: "tengu_quartz_lantern",
        enabled: false,
        note: "Official default false; Write/Edit skip the single-file git diff attachment even when NYAN_REMOTE is truthy.",
    },
    FeatureSwitch {
        flag: FeatureFlag::PromptSuggestion,
        cc_growthbook_name: "tengu_chomp_inflection",
        enabled: false,
        note: "Official default false; NYAN_ENABLE_PROMPT_SUGGESTION still opts in explicitly and the settings toggle stays hidden without it.",
    },
    FeatureSwitch {
        flag: FeatureFlag::ChannelAllowlistLedger,
        cc_growthbook_name: "tengu_harbor_ledger",
        enabled: false,
        note: "Official default is an empty ledger, so no plugin channel is allowlisted; the org policy allowedChannelPlugins list remains the source-controlled way to approve channels.",
    },
    FeatureSwitch {
        flag: FeatureFlag::TeamMemory,
        cc_growthbook_name: "tengu_herring_clock",
        enabled: false,
        note: "Official default false; team-memory paths stay unrecognized and the auto-memory setting alone cannot enable them.",
    },
    FeatureSwitch {
        flag: FeatureFlag::PluginHintRecommendation,
        cc_growthbook_name: "tengu_lapis_finch",
        enabled: false,
        note: "Official default false; Bash/PowerShell plugin hints are stripped without ever reaching the startup recommendation dialog.",
    },
    FeatureSwitch {
        flag: FeatureFlag::DesktopUpsell,
        cc_growthbook_name: "tengu_desktop_upsell",
        enabled: false,
        note: "Official default disables both the shortcut tip and the startup dialog, so the desktop handoff upsell never renders.",
    },
    FeatureSwitch {
        flag: FeatureFlag::EmergencyTip,
        cc_growthbook_name: "tengu-top-of-feed-tip",
        enabled: false,
        note: "Official default is an empty tip, so LogoV2 renders no top-of-feed notice; the dynamic config is the only upstream producer and Nyan has no source-controlled tip payload.",
    },
    FeatureSwitch {
        flag: FeatureFlag::IronGateClosed,
        cc_growthbook_name: "tengu_iron_gate_closed",
        enabled: true,
        note: "Official default true: an unavailable auto-mode classifier denies with retry guidance (fail closed) instead of falling back to the untouched ask result.",
    },
    FeatureSwitch {
        flag: FeatureFlag::MemoryCorrectionHint,
        cc_growthbook_name: "tengu_amber_prism",
        enabled: false,
        note: "CC's own fallback is false (utils/messages.ts:188 getFeatureValue_CACHED_MAY_BE_STALE('tengu_amber_prism', false)), so rejection/cancellation copy carries no memory hint unless this switch is flipped.",
    },
    FeatureSwitch {
        flag: FeatureFlag::WorkflowsEnabled,
        cc_growthbook_name: "tengu_workflows_enabled",
        enabled: false,
        note: "Official GrowthBook fallback is true. The Workflow tool is not ported, so this source-controlled switch stays off; isDynamicWorkflowsEnabled / isUltracodeAvailable follow it.",
    },
    FeatureSwitch {
        flag: FeatureFlag::DisableKeepaliveOnEconnreset,
        cc_growthbook_name: "tengu_disable_keepalive_on_econnreset",
        enabled: false,
        note: "CC's own fallback is false (withRetry.ts:220-223 getFeatureValue_CACHED_MAY_BE_STALE(..., false)): a stale ECONNRESET/EPIPE connection does not call disableKeepAlive unless this switch is flipped.",
    },
    FeatureSwitch {
        flag: FeatureFlag::SlimSubagentAgentsMd,
        cc_growthbook_name: "tengu_slim_subagent_claudemd",
        enabled: true,
        note: "Official fallback true: agents whose definition sets omitClaudeMd drop the AGENTS.md user context unless the caller overrides it.",
    },
    FeatureSwitch {
        flag: FeatureFlag::UniversalToolResultMerge,
        cc_growthbook_name: "tengu_chair_sermon",
        enabled: false,
        note: "Official fallback false: only text-only follow-ups fold into a preceding tool_result; attachments stay separate user messages.",
    },
    FeatureSwitch {
        flag: FeatureFlag::DisableBypassPermissionsMode,
        cc_growthbook_name: "tengu_disable_bypass_permissions_mode",
        enabled: false,
        note: "Official fallback false: bypass-permissions mode is only disabled by the permissions.disableBypassPermissionsMode setting.",
    },
    FeatureSwitch {
        flag: FeatureFlag::OfficialMarketplaceGitFallback,
        cc_growthbook_name: "tengu_plugin_official_mkt_git_fallback",
        enabled: true,
        note: "Official fallback true: when the official marketplace GCS mirror fails, refresh falls back to git.",
    },
    FeatureSwitch {
        flag: FeatureFlag::Advisor,
        cc_growthbook_name: "tengu_sage_compass",
        enabled: false,
        note: "Official fallback is an empty config, so the advisor tool and /advisor stay disabled; enabling this switch also lets the user configure the advisor model.",
    },
];

pub fn feature_enabled(flag: FeatureFlag) -> bool {
    #[cfg(test)]
    if let Some(enabled) = test_override(flag) {
        return enabled;
    }
    FEATURE_SWITCHES
        .iter()
        .find(|switch| switch.flag == flag)
        .is_some_and(|switch| switch.enabled)
}

#[cfg(test)]
thread_local! {
    static TEST_OVERRIDES: std::cell::RefCell<std::collections::HashMap<FeatureFlag, bool>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

#[cfg(test)]
fn test_override(flag: FeatureFlag) -> Option<bool> {
    TEST_OVERRIDES.with(|overrides| overrides.borrow().get(&flag).copied())
}

/// Test-only: flips `flag` on the current thread until the guard drops, so a
/// test can exercise the non-default arm of a source-controlled switch.
#[cfg(test)]
pub(crate) fn override_for_test(flag: FeatureFlag, enabled: bool) -> FeatureOverrideGuard {
    let previous = TEST_OVERRIDES.with(|overrides| overrides.borrow_mut().insert(flag, enabled));
    FeatureOverrideGuard { flag, previous }
}

#[cfg(test)]
pub(crate) struct FeatureOverrideGuard {
    flag: FeatureFlag,
    previous: Option<bool>,
}

#[cfg(test)]
impl Drop for FeatureOverrideGuard {
    fn drop(&mut self) {
        TEST_OVERRIDES.with(|overrides| {
            let mut overrides = overrides.borrow_mut();
            match self.previous {
                Some(enabled) => overrides.insert(self.flag, enabled),
                None => overrides.remove(&self.flag),
            };
        });
    }
}

pub fn feature_switch_by_growthbook_name(name: &str) -> Option<&'static FeatureSwitch> {
    FEATURE_SWITCHES
        .iter()
        .find(|switch| switch.cc_growthbook_name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardcoded_feature_switch_table_covers_known_growthbook_gates() {
        let names = FEATURE_SWITCHES
            .iter()
            .map(|switch| switch.cc_growthbook_name)
            .collect::<std::collections::BTreeSet<_>>();

        for expected in [
            "tengu_immediate_model_command",
            "tengu_keybinding_customization_release",
            "tengu_amber_flint",
            "tengu_harbor",
            "tengu_harbor_permissions",
            "tengu_kairos_cron",
            "tengu_kairos_cron_durable",
            "tengu_surreal_dali",
            "tengu_glacier_2xr",
            "tengu_streaming_tool_execution2",
            "tengu_scratch",
            "tengu_moth_copse",
            "tengu_coral_fern",
            "tengu_otk_slot_v1",
            "tengu_plum_vx3",
            "tengu_skill_dynamic_discovery",
            "tengu_iron_gate_closed",
            "tengu_attribution_header",
            "tengu_workflows_enabled",
            "NEW_INIT",
            "BREAK_CACHE_COMMAND",
            "COORDINATOR_MODE",
            "PROACTIVE",
            "KAIROS",
            "BRIDGE_MODE",
            "MCP_SKILLS",
            "EXPERIMENTAL_SKILL_SEARCH",
            "AGENT_MEMORY_SNAPSHOT",
        ] {
            assert!(names.contains(expected), "missing {expected}");
        }
    }

    #[test]
    fn growthbook_name_lookup_reads_source_controlled_switches_only() {
        let switch = feature_switch_by_growthbook_name("tengu_amber_flint")
            .expect("agent swarms switch should exist");
        assert_eq!(switch.flag, FeatureFlag::AgentSwarmsExternalKillswitch);
        assert!(feature_enabled(FeatureFlag::AgentSwarmsExternalKillswitch));
        assert!(!feature_enabled(FeatureFlag::ChannelsEnabled));
        assert!(!feature_enabled(FeatureFlag::ChannelPermissionRelay));
        assert!(!feature_enabled(FeatureFlag::ImmediateModelCommand));
    }

    /// CC `constants/system.ts:56` reads `tengu_attribution_header` with
    /// fallback `true`. Production stays off via the env default of `0`.
    #[test]
    fn attribution_header_defaults_to_the_official_fallback_true() {
        let switch = feature_switch_by_growthbook_name("tengu_attribution_header")
            .expect("attribution header switch should exist");
        assert_eq!(switch.flag, FeatureFlag::AttributionHeader);
        assert!(feature_enabled(FeatureFlag::AttributionHeader));
    }

    /// CC `utils/permissions/permissions.ts:846-852` reads
    /// `tengu_iron_gate_closed` with default `true`, so the auto-mode
    /// classifier fails closed unless the gate is explicitly turned off.
    #[test]
    fn iron_gate_closed_defaults_to_the_official_fail_closed_value() {
        let switch = feature_switch_by_growthbook_name("tengu_iron_gate_closed")
            .expect("iron gate switch should exist");
        assert_eq!(switch.flag, FeatureFlag::IronGateClosed);
        assert!(feature_enabled(FeatureFlag::IronGateClosed));
    }
}
