//! Hardcoded feature-switch collection for CC GrowthBook-controlled gates.
//!
//! Maps to CC GrowthBook feature reads such as
//! `getFeatureValue_CACHED_MAY_BE_STALE(...)` and
//! `getFeatureValue_CACHED_WITH_REFRESH(...)`, but intentionally does not
//! implement cloud delivery, cache parsing, refresh windows, or config override
//! ingestion. Cometix keeps a single source-controlled switch table instead.

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
    /// Maps to CC GrowthBook voice-mode killswitch `tengu_amber_quartz_disabled`.
    VoiceModeDisabledKillswitch,
    /// Maps to CC GrowthBook channels runtime gate `tengu_harbor`.
    ChannelsEnabled,
    /// Maps to CC GrowthBook channel permission-relay gate `tengu_harbor_permissions`.
    ChannelPermissionRelay,
    /// Maps to CC GrowthBook `tengu_kairos_cron`.
    KairosCron,
    /// Maps to CC GrowthBook `tengu_kairos_cron_durable`.
    DurableCron,
    /// Maps to CC GrowthBook `tengu_slate_heron`.
    TimeBasedMicrocompact,
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
    /// Maps to CC `feature('KAIROS_PUSH_NOTIFICATION')` build feature; every CC
    /// reader pairs it as `feature('KAIROS') || feature('KAIROS_PUSH_NOTIFICATION')`
    /// (`tools/ConfigTool/supportedSettings.ts:164`, `tools.ts:46`,
    /// `components/Settings/Config.tsx:766/:791`).
    KairosPushNotification,
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
    /// Maps to CC GrowthBook `tengu_ccr_bridge` Remote Control entitlement.
    RemoteControlEntitlement,
    /// Maps to CC GrowthBook `tengu_hawthorn_steeple` tool-result content-replacement gate.
    ToolResultContentReplacement,
    /// Maps to CC GrowthBook `tengu_hawthorn_window` per-message budget override.
    PerMessageBudgetLimitOverride,
    /// Maps to CC GrowthBook `tengu_prompt_cache_1h_config`.
    /// @cometix offset: final 1P `ttl: "1h"` switch. The allowlist itself is
    /// settings.json `promptCache1h.allowlist`. Vertex / Foundry / Bedrock
    /// keep the official `should1hCacheTTL` path.
    PromptCache1hConfig,
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
/// Change this table, not runtime config/cache parsing, when Cometix needs a
/// different behavior for an upstream GrowthBook-controlled feature.
pub const FEATURE_SWITCHES: &[FeatureSwitch] = &[
    FeatureSwitch {
        flag: FeatureFlag::ImmediateModelCommand,
        cc_growthbook_name: "tengu_immediate_model_command",
        enabled: false,
        note: "External /model immediate command gate; the anthropic_internal build remains enabled separately.",
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
        flag: FeatureFlag::VoiceModeDisabledKillswitch,
        cc_growthbook_name: "tengu_amber_quartz_disabled",
        enabled: false,
        note: "Voice-mode emergency-off GrowthBook flag; default false means voice UI is not killed when the build feature and auth allow it.",
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
        flag: FeatureFlag::TimeBasedMicrocompact,
        cc_growthbook_name: "tengu_slate_heron",
        enabled: false,
        note: "Time-based microcompact default remains disabled unless a local seam opts in explicitly.",
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
        enabled: false,
        note: "Deferred-tool delta announcements use the pre-gate available-deferred-tools wording by default.",
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
        note: "Build feature is present; /init still requires the anthropic_internal build or CLAUDE_CODE_NEW_INIT at runtime.",
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
        note: "Build feature for coordinator system prompt / mode detection; runtime still requires CLAUDE_CODE_COORDINATOR_MODE.",
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
        flag: FeatureFlag::KairosPushNotification,
        cc_growthbook_name: "KAIROS_PUSH_NOTIFICATION",
        // Build feature, not GrowthBook: `scripts/build.ts:111` lists
        // `KAIROS_PUSH_NOTIFICATION: isDev`, i.e. OFF in production.
        enabled: false,
        note: "Build feature for the push-notification-only Kairos slice; CC always reads it OR-ed with KAIROS. The Config push-notification settings (supportedSettings.ts:164-185) honour the OR; the other two CC readers have no Rust counterpart yet (PushNotificationTool is unported, Settings/Config.tsx:766/:791 is not in components/settings/config.rs).",
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
        note: "Official default false; CLAUDE_CODE_BRIEF and assistant mode grant entitlement without it.",
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
        note: "Read-time nested .claude/skills discovery is locally available; remote cohort delivery is represented by this explicit source-controlled seam.",
    },
    FeatureSwitch {
        flag: FeatureFlag::BridgeMode,
        cc_growthbook_name: "BRIDGE_MODE",
        // CC ships BRIDGE_MODE on in production builds (scripts/build.ts:41
        // "Generally available (ON in production)").
        enabled: true,
        note: "Remote Control build gate mirrors CC production (on); pill/dialog reachability still requires the isBridgeEnabled entitlement and a replBridgeEnabled producer (absent in Cometix — no bridge transport or settings-screen toggle).",
    },
    FeatureSwitch {
        flag: FeatureFlag::McpSkills,
        cc_growthbook_name: "MCP_SKILLS",
        enabled: true,
        note: "CC 2.1.88 build enables MCP prompt commands as Skill-tool skills; listing and invocation share this seam.",
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
        note: "Official default is an empty override map, so the MCP output cap falls through to the hardcoded default; the switch stays off because Cometix has no source-controlled per-tool override payload.",
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
        note: "Official default false; eager_input_streaming still opts in through CLAUDE_CODE_ENABLE_FINE_GRAINED_TOOL_STREAMING on first-party Anthropic base URLs.",
    },
    FeatureSwitch {
        flag: FeatureFlag::FileReadLimitsOverride,
        cc_growthbook_name: "tengu_amber_wren",
        enabled: false,
        note: "Official default is an empty override map, so Read keeps MAX_OUTPUT_SIZE / DEFAULT_MAX_OUTPUT_TOKENS; CLAUDE_CODE_FILE_READ_MAX_OUTPUT_TOKENS still overrides the token cap.",
    },
    FeatureSwitch {
        flag: FeatureFlag::RemoteControlEntitlement,
        cc_growthbook_name: "tengu_ccr_bridge",
        enabled: false,
        note: "Official default false; the per-account Remote Control entitlement cannot be resolved without the GrowthBook runtime, so the bridge stays unreachable even for claude.ai subscribers.",
    },
    FeatureSwitch {
        flag: FeatureFlag::ToolResultContentReplacement,
        cc_growthbook_name: "tengu_hawthorn_steeple",
        enabled: false,
        note: "Official default false: provisionContentReplacementState returns None, so the aggregate per-message budget pass never runs and tool results are delivered unreplaced.",
    },
    FeatureSwitch {
        flag: FeatureFlag::PerMessageBudgetLimitOverride,
        cc_growthbook_name: "tengu_hawthorn_window",
        enabled: false,
        note: "Official default is null, so the per-message aggregate budget stays at MAX_TOOL_RESULTS_PER_MESSAGE_CHARS; the switch stays off because Cometix has no source-controlled window payload.",
    },
    FeatureSwitch {
        flag: FeatureFlag::AttributionHeader,
        cc_growthbook_name: "tengu_attribution_header",
        enabled: true,
        note: "Official fallback is true (constants/system.ts:56 getFeatureValue_CACHED_MAY_BE_STALE('tengu_attribution_header', true)). Production still stays off because CLAUDE_CODE_ATTRIBUTION_HEADER defaults to 0; flip this switch to kill the header even when the env is enabled.",
    },
    FeatureSwitch {
        flag: FeatureFlag::PromptCache1hConfig,
        cc_growthbook_name: "tengu_prompt_cache_1h_config",
        enabled: false,
        note: "Official default is an empty config object. @cometix offset: first-party 1h master switch (default off). The query-source allowlist is settings.json promptCache1h.allowlist, not GrowthBook.",
    },
    FeatureSwitch {
        flag: FeatureFlag::RemoteGitDiff,
        cc_growthbook_name: "tengu_quartz_lantern",
        enabled: false,
        note: "Official default false; Write/Edit skip the single-file git diff attachment even when CLAUDE_CODE_REMOTE is truthy.",
    },
    FeatureSwitch {
        flag: FeatureFlag::PromptSuggestion,
        cc_growthbook_name: "tengu_chomp_inflection",
        enabled: false,
        note: "Official default false; CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION still opts in explicitly and the settings toggle stays hidden without it.",
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
        note: "Official default is an empty tip, so LogoV2 renders no top-of-feed notice; the dynamic config is the only upstream producer and Cometix has no source-controlled tip payload.",
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
];

pub fn feature_enabled(flag: FeatureFlag) -> bool {
    FEATURE_SWITCHES
        .iter()
        .find(|switch| switch.flag == flag)
        .is_some_and(|switch| switch.enabled)
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
            "tengu_amber_quartz_disabled",
            "tengu_harbor",
            "tengu_harbor_permissions",
            "tengu_kairos_cron",
            "tengu_kairos_cron_durable",
            "tengu_slate_heron",
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
        assert!(!feature_enabled(FeatureFlag::VoiceModeDisabledKillswitch));
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
