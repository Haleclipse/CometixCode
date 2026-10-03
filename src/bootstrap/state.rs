//! Process bootstrap state.
//! Maps to CC `bootstrap/state.ts`.

#[cfg(test)]
use crate::utils::test_env::TestStateLock;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{LazyLock, RwLock};
use unicode_normalization::UnicodeNormalization;

/// Maps to: CC `bootstrap/state.ts` `ChannelEntry`.
///
/// `dev` is true for entries that came from
/// `--dangerously-load-development-channels`; official callers use the per-entry
/// flag rather than a session-wide bit when checking allowlist bypass behavior.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ChannelEntry {
    Plugin {
        name: String,
        marketplace: String,
        dev: bool,
    },
    Server {
        name: String,
        dev: bool,
    },
}

impl ChannelEntry {
    pub fn plugin(name: impl Into<String>, marketplace: impl Into<String>, dev: bool) -> Self {
        Self::Plugin {
            name: name.into(),
            marketplace: marketplace.into(),
            dev,
        }
    }

    pub fn server(name: impl Into<String>, dev: bool) -> Self {
        Self::Server {
            name: name.into(),
            dev,
        }
    }

    pub fn is_dev(&self) -> bool {
        match self {
            Self::Plugin { dev, .. } | Self::Server { dev, .. } => *dev,
        }
    }
}

static FLAG_SETTINGS_PATH: LazyLock<RwLock<Option<PathBuf>>> = LazyLock::new(|| RwLock::new(None));
/// Maps to: CC `bootstrap/state.ts:120,346` `STATE.lastClassifierRequests`.
static LAST_CLASSIFIER_REQUESTS: LazyLock<RwLock<Option<Vec<serde_json::Value>>>> =
    LazyLock::new(|| RwLock::new(None));
/// Maps to: CC `bootstrap/state.ts:123,347` `STATE.cachedClaudeMdContent`.
static CACHED_CLAUDE_MD_CONTENT: LazyLock<RwLock<Option<String>>> =
    LazyLock::new(|| RwLock::new(None));
/// Maps to: CC `bootstrap/state.ts:198,389` `STATE.isRemoteMode`.
/// Remote startup's setter caller is not yet ported; never infer it from env.
static IS_REMOTE_MODE: LazyLock<RwLock<bool>> = LazyLock::new(|| RwLock::new(false));

/// Maps to: CC `bootstrap/state.ts:1631-1633` `getIsRemoteMode`.
pub fn get_is_remote_mode() -> bool {
    *IS_REMOTE_MODE.read().unwrap_or_else(|p| p.into_inner())
}

/// Maps to: CC `bootstrap/state.ts:1635-1637` `setIsRemoteMode`.
pub fn set_is_remote_mode(value: bool) {
    *IS_REMOTE_MODE.write().unwrap_or_else(|p| p.into_inner()) = value;
}

/// Maps to: CC `bootstrap/state.ts:1199-1201` `setLastClassifierRequests`.
pub fn set_last_classifier_requests(requests: Option<Vec<serde_json::Value>>) {
    *LAST_CLASSIFIER_REQUESTS
        .write()
        .unwrap_or_else(|p| p.into_inner()) = requests;
}

/// Maps to: CC `bootstrap/state.ts:1203-1205` `getLastClassifierRequests`.
pub fn get_last_classifier_requests() -> Option<Vec<serde_json::Value>> {
    LAST_CLASSIFIER_REQUESTS
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

/// Maps to: CC `bootstrap/state.ts:1207-1209` `setCachedClaudeMdContent`.
pub fn set_cached_claude_md_content(content: Option<String>) {
    *CACHED_CLAUDE_MD_CONTENT
        .write()
        .unwrap_or_else(|p| p.into_inner()) = content;
}

/// Maps to: CC `bootstrap/state.ts:1211-1213` `getCachedClaudeMdContent`.
pub fn get_cached_claude_md_content() -> Option<String> {
    CACHED_CLAUDE_MD_CONTENT
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}
static FLAG_SETTINGS_INLINE: LazyLock<RwLock<Option<serde_json::Value>>> =
    LazyLock::new(|| RwLock::new(None));
/// Maps to: CC `bootstrap/state.ts:1158-1164` `STATE.oauthTokenFromFd`.
/// Outer `None` is JavaScript `undefined`; inner `None` is cached `null`.
static OAUTH_TOKEN_FROM_FD: LazyLock<RwLock<Option<Option<String>>>> =
    LazyLock::new(|| RwLock::new(None));
/// Maps to: CC `bootstrap/state.ts:1166-1172` `STATE.apiKeyFromFd`.
static API_KEY_FROM_FD: LazyLock<RwLock<Option<Option<String>>>> =
    LazyLock::new(|| RwLock::new(None));
static CLI_AGENTS_JSON: LazyLock<RwLock<Option<serde_json::Value>>> =
    LazyLock::new(|| RwLock::new(None));
static INLINE_PLUGINS: LazyLock<RwLock<Vec<PathBuf>>> = LazyLock::new(|| RwLock::new(Vec::new()));
static USE_COWORK_PLUGINS: LazyLock<RwLock<bool>> = LazyLock::new(|| RwLock::new(false));
static ADDITIONAL_DIRECTORIES_FOR_CLAUDE_MD: LazyLock<RwLock<Vec<PathBuf>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));
/// Maps to CC `STATE.allowedSettingSources`; policy/flag are added by the
/// settings owner and therefore are not stored in this user-selectable list.
static ALLOWED_SETTING_SOURCES: LazyLock<RwLock<Vec<String>>> = LazyLock::new(|| {
    RwLock::new(vec![
        "userSettings".to_string(),
        "projectSettings".to_string(),
        "localSettings".to_string(),
    ])
});
static ALLOWED_CHANNELS: LazyLock<RwLock<Vec<ChannelEntry>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));
static AGENT_COLOR_MAP: LazyLock<
    RwLock<HashMap<String, crate::tools::agent_tool::agent_color_manager::AgentColorName>>,
> = LazyLock::new(|| RwLock::new(HashMap::new()));
static HAS_DEV_CHANNELS: LazyLock<RwLock<bool>> = LazyLock::new(|| RwLock::new(false));
/// Maps to: CC `bootstrap/state.ts:79` `STATE.userMsgOptIn`.
///
/// CC's opt-in actions live in `main.tsx` (`--brief`, `defaultView: 'chat'`,
/// `/brief`, `--tools`, `CLAUDE_CODE_BRIEF`). Only the env-var path exists in
/// Cometix, and `maybeActivateBrief` (`main.tsx:6524-6553`) sets the flag from
/// it, so the initial value is derived here.
static USER_MSG_OPT_IN: LazyLock<RwLock<bool>> = LazyLock::new(|| {
    RwLock::new(crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_BRIEF").as_deref(),
    ))
});
/// Maps to: CC `bootstrap/state.ts:82` `STATE.questionPreviewFormat`.
///
/// CC seeds this in `main.tsx:1153-1165`: the env override wins, otherwise
/// every non-`sdk-`/desktop/CCR client type gets `markdown`. Cometix's client
/// type is always `cli`, so the fallback is unconditional here.
static QUESTION_PREVIEW_FORMAT: LazyLock<RwLock<Option<QuestionPreviewFormat>>> =
    LazyLock::new(|| {
        RwLock::new(
            match crate::utils::process_env::var("CLAUDE_CODE_QUESTION_PREVIEW_FORMAT").unwrap_or_default()
                .as_str()
            {
                "html" => Some(QuestionPreviewFormat::Html),
                _ => Some(QuestionPreviewFormat::Markdown),
            },
        )
    });
/// Maps to: CC `bootstrap/state.ts` `registeredHooks` — the
/// `RegisteredHookMatcher[]` channel (plugin command hooks and SDK callback
/// hooks share it).
static REGISTERED_HOOKS: LazyLock<RwLock<Option<crate::schemas::hooks::RegisteredHooks>>> =
    LazyLock::new(|| RwLock::new(None));
static SESSION_ID: LazyLock<RwLock<String>> =
    LazyLock::new(|| RwLock::new(uuid::Uuid::new_v4().to_string()));
/// Maps to: CC `bootstrap/state.ts` `sessionProjectDir`.
static SESSION_PROJECT_DIR: LazyLock<RwLock<Option<PathBuf>>> = LazyLock::new(|| RwLock::new(None));
/// Maps to: CC `bootstrap/state.ts` `parentSessionId`.
static PARENT_SESSION_ID: LazyLock<RwLock<Option<String>>> = LazyLock::new(|| RwLock::new(None));
static SESSION_CREATED_TEAMS: LazyLock<RwLock<HashSet<String>>> =
    LazyLock::new(|| RwLock::new(HashSet::new()));
/// Maps to CC `bootstrap/state.ts:300` `isInteractive`.
///
/// DEVIATION(TEST-DEFAULT): CC seeds `false` and `main.tsx:1119-1120`
/// immediately overwrites it, so nothing in a CC process reads the seed except
/// code that runs before `main()`. The production computation lives at the same
/// point here ([`crate::main::initialize_is_interactive`], called from
/// `entrypoints/cli.rs#run` before argv is parsed), so the seed is likewise
/// unobservable in a real run — but a unit test never calls it, and the port's
/// tests overwhelmingly assert the interactive branch. Seeding `true` keeps
/// that the default a test opts OUT of rather than one every interactive test
/// must opt in to.
static IS_INTERACTIVE: LazyLock<RwLock<bool>> = LazyLock::new(|| RwLock::new(true));
/// Maps to CC `bootstrap/state.ts` `STATE.kairosActive`.
static KAIROS_ACTIVE: LazyLock<RwLock<bool>> = LazyLock::new(|| RwLock::new(false));
/// Maps to: CC bootstrap/state.ts:792-824 scroll drain debounce and poll.
const SCROLL_DRAIN_IDLE_MS: u64 = 150;
static SCROLL_DRAIN_TIMER: LazyLock<std::sync::Mutex<Option<std::time::Instant>>> =
    LazyLock::new(|| std::sync::Mutex::new(None));
/// Maps to: CC bootstrap/state.ts#markScrollActivity. Native monotonic deadline
/// carries the resettable unref timer without keeping the process alive.
pub fn mark_scroll_activity() {
    *SCROLL_DRAIN_TIMER.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(std::time::Instant::now() + std::time::Duration::from_millis(SCROLL_DRAIN_IDLE_MS));
}
/// Maps to: CC bootstrap/state.ts#getIsScrollDraining.
pub fn get_is_scroll_draining() -> bool {
    let mut timer = SCROLL_DRAIN_TIMER.lock().unwrap_or_else(|e| e.into_inner());
    if timer.is_some_and(|deadline| std::time::Instant::now() < deadline) {
        true
    } else {
        *timer = None;
        false
    }
}
/// Maps to: CC bootstrap/state.ts#waitForScrollIdle.
pub async fn wait_for_scroll_idle() {
    while get_is_scroll_draining() {
        tokio::time::sleep(std::time::Duration::from_millis(SCROLL_DRAIN_IDLE_MS)).await;
    }
}

static ORIGINAL_CWD: LazyLock<RwLock<PathBuf>> = LazyLock::new(|| {
    // Harness pin, test builds only: `projectSettings` roots at this value
    // (`${cwd}/.claude/settings.json`), and it is process state no recipe-
    // scoped env can otherwise reach. `just _prep` rebuilds mutable config in
    // `target/test-home`, serializes its canonical trust JSON, and points this
    // value at the tracked `tests/fixtures/isolated-project` instead of whatever
    // repository the tests happen to run in. `IsolatedProjectSettings` remains
    // the per-test override. This is environment injection, not a behavior
    // fork: the value only changes where the cwd APPEARS to be, exactly like
    // CLAUDE_CONFIG_DIR.

    #[cfg(test)]
    if let Some(pinned) = crate::utils::process_env::var("COMETIX_TEST_PROJECT_DIR") {
        if !pinned.is_empty() {
            let pinned = PathBuf::from(pinned);
            return RwLock::new(PathBuf::from(
                pinned
                    .canonicalize()
                    .unwrap_or(pinned)
                    .to_string_lossy()
                    .nfc()
                    .collect::<String>(),
            ));
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    // CC bootstrap/state.ts:271-274 normalizes both realpath and fallback cwd.
    RwLock::new(PathBuf::from(
        cwd.canonicalize()
            .unwrap_or(cwd)
            .to_string_lossy()
            .nfc()
            .collect::<String>(),
    ))
});
/// Maps to: CC `bootstrap/state.ts:68,297,838-850` `mainLoopModelOverride`.
///
/// The outer option is JavaScript `undefined`; the inner option is the
/// source's explicit `null` model setting.
static MAIN_LOOP_MODEL_OVERRIDE: LazyLock<RwLock<Option<Option<String>>>> =
    LazyLock::new(|| RwLock::new(None));
/// Maps to: CC `bootstrap/state.ts:69,298,842-844,852-854` `initialMainLoopModel`
/// — the user-specified model setting captured once at launch
/// (`main.tsx:3068`) and read back by `modelOptions.ts:490`, so a model pinned
/// at startup still appears in the picker after `/model` clears it.
///
/// A single option, unlike `MAIN_LOOP_MODEL_OVERRIDE`: the source types this
/// field `ModelSetting` (two states, `null` = unset), not
/// `ModelSetting | undefined`.
static INITIAL_MAIN_LOOP_MODEL: LazyLock<RwLock<Option<String>>> =
    LazyLock::new(|| RwLock::new(None));
/// Maps to: CC `bootstrap/state.ts` `sessionTrustAccepted`.
static SESSION_TRUST_ACCEPTED: LazyLock<RwLock<bool>> = LazyLock::new(|| RwLock::new(false));
/// Maps to: CC `bootstrap/state.ts` `sessionPersistenceDisabled`.
static SESSION_PERSISTENCE_DISABLED: LazyLock<RwLock<bool>> = LazyLock::new(|| RwLock::new(false));
/// Maps to: CC `bootstrap/state.ts` `hasExitedPlanMode`.
static HAS_EXITED_PLAN_MODE: LazyLock<RwLock<bool>> = LazyLock::new(|| RwLock::new(false));
/// Maps to: CC `bootstrap/state.ts` `needsPlanModeExitAttachment`.
static NEEDS_PLAN_MODE_EXIT_ATTACHMENT: LazyLock<RwLock<bool>> =
    LazyLock::new(|| RwLock::new(false));
/// Maps to: CC `bootstrap/state.ts` `needsAutoModeExitAttachment`.
static NEEDS_AUTO_MODE_EXIT_ATTACHMENT: LazyLock<RwLock<bool>> =
    LazyLock::new(|| RwLock::new(false));
/// Maps to: CC `bootstrap/state.ts:1639-1654` `systemPromptSectionCache`.
/// The outer `Option` returned by the accessor distinguishes an uncached
/// section from a cached section whose computed value is `null`/`None`.
static SYSTEM_PROMPT_SECTION_CACHE: LazyLock<RwLock<HashMap<String, Option<String>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));
/// Maps to CC `bootstrap/state.ts:1501-1563` invoked-skill preservation state.
static INVOKED_SKILLS: LazyLock<RwLock<Vec<(String, InvokedSkillInfo)>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));
#[cfg(test)]
pub static TEST_INVOKED_SKILLS_LOCK: LazyLock<TestStateLock> = LazyLock::new(TestStateLock::new);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvokedSkillInfo {
    pub skill_name: String,
    pub skill_path: String,
    pub content: String,
    pub invoked_at_ms: i64,
    pub agent_id: Option<String>,
}

/// Maps to: CC `bootstrap/state.ts:1641-1643`
/// `getSystemPromptSectionCache()` lookup semantics.
pub fn get_system_prompt_section_cache_entry(name: &str) -> Option<Option<String>> {
    SYSTEM_PROMPT_SECTION_CACHE
        .read()
        .ok()
        .and_then(|cache| cache.get(name).cloned())
}

/// Maps to: CC `bootstrap/state.ts:1645-1650`
/// `setSystemPromptSectionCacheEntry(...)`.
pub fn set_system_prompt_section_cache_entry(name: impl Into<String>, value: Option<String>) {
    if let Ok(mut cache) = SYSTEM_PROMPT_SECTION_CACHE.write() {
        cache.insert(name.into(), value);
    }
}

/// Maps to: CC `bootstrap/state.ts:838-840#getMainLoopModelOverride`.
pub fn get_main_loop_model_override() -> Option<Option<String>> {
    MAIN_LOOP_MODEL_OVERRIDE
        .read()
        .map(|model| model.clone())
        .unwrap_or(None)
}

/// Maps to: CC `bootstrap/state.ts:842-844#getInitialMainLoopModel`.
pub fn get_initial_main_loop_model() -> Option<String> {
    INITIAL_MAIN_LOOP_MODEL
        .read()
        .map(|model| model.clone())
        .unwrap_or(None)
}

/// Maps to: CC `bootstrap/state.ts:846-850#setMainLoopModelOverride`.
pub fn set_main_loop_model_override(model: Option<Option<String>>) {
    if let Ok(mut current) = MAIN_LOOP_MODEL_OVERRIDE.write() {
        *current = model;
    }
}

/// Maps to: CC `bootstrap/state.ts:852-854#setInitialMainLoopModel`.
pub fn set_initial_main_loop_model(model: Option<String>) {
    if let Ok(mut current) = INITIAL_MAIN_LOOP_MODEL.write() {
        *current = model;
    }
}

/// Maps to: CC `bootstrap/state.ts:1652-1654`
/// `clearSystemPromptSectionState()`.
pub fn clear_system_prompt_section_state() {
    if let Ok(mut cache) = SYSTEM_PROMPT_SECTION_CACHE.write() {
        cache.clear();
    }
}

#[cfg(test)]
pub(crate) fn system_prompt_section_cache_len_for_test() -> usize {
    SYSTEM_PROMPT_SECTION_CACHE
        .read()
        .map(|cache| cache.len())
        .unwrap_or(0)
}

/// Maps to CC `bootstrap/state.ts:1510-1523` `addInvokedSkill(...)`.
pub fn add_invoked_skill(
    skill_name: impl Into<String>,
    skill_path: impl Into<String>,
    content: impl Into<String>,
    agent_id: Option<&str>,
) {
    let skill_name = skill_name.into();
    let normalized_agent = agent_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let key = format!("{}:{skill_name}", normalized_agent.as_deref().unwrap_or(""));
    if let Ok(mut skills) = INVOKED_SKILLS.write() {
        let info = InvokedSkillInfo {
            skill_name,
            skill_path: skill_path.into(),
            content: content.into(),
            invoked_at_ms: chrono::Utc::now().timestamp_millis(),
            agent_id: normalized_agent,
        };
        if let Some((_, existing)) = skills
            .iter_mut()
            .find(|(existing_key, _)| existing_key == &key)
        {
            *existing = info;
        } else {
            skills.push((key, info));
        }
    }
}

/// Maps to CC `bootstrap/state.ts:1530-1541`
/// `getInvokedSkillsForAgent(...)`.
pub fn get_invoked_skills_for_agent(agent_id: Option<&str>) -> Vec<InvokedSkillInfo> {
    let normalized_agent = agent_id.map(str::trim).filter(|value| !value.is_empty());
    INVOKED_SKILLS
        .read()
        .map(|skills| {
            skills
                .iter()
                .map(|(_, skill)| skill)
                .filter(|skill| skill.agent_id.as_deref() == normalized_agent)
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// Maps to CC `bootstrap/state.ts:1543-1555` `clearInvokedSkills(...)`.
pub fn clear_invoked_skills(preserved_agent_ids: Option<&HashSet<String>>) {
    if let Ok(mut skills) = INVOKED_SKILLS.write() {
        let Some(preserved) = preserved_agent_ids.filter(|ids| !ids.is_empty()) else {
            skills.clear();
            return;
        };
        skills.retain(|(_, skill)| {
            skill
                .agent_id
                .as_ref()
                .is_some_and(|agent_id| preserved.contains(agent_id))
        });
    }
}

/// Maps to CC `bootstrap/state.ts:1557-1563`
/// `clearInvokedSkillsForAgent(...)`.
pub fn clear_invoked_skills_for_agent(agent_id: &str) {
    if let Ok(mut skills) = INVOKED_SKILLS.write() {
        skills.retain(|(_, skill)| skill.agent_id.as_deref() != Some(agent_id));
    }
}

/// Return the active `--settings` path, if one was provided.
///
/// Maps to: CC `bootstrap/state.ts:getFlagSettingsPath`.
pub fn get_flag_settings_path() -> Option<PathBuf> {
    FLAG_SETTINGS_PATH.read().ok().and_then(|path| path.clone())
}

/// Set the active `--settings` path for this process.
///
/// Maps to: CC `bootstrap/state.ts:setFlagSettingsPath`.
/// Every setter that changes what `settings.ts` merges MUST invalidate the
/// settings cache: `get_settings_with_errors` memoizes the merged result for
/// the session, so a new flag source or a new allow-list would otherwise be
/// invisible to every later read. CC never hits this because its flag settings
/// and source allow-list are fixed by CLI parsing before the first read; the
/// Rust setters are reachable at any time (tests drive them directly), so the
/// invalidation has to be explicit here.
pub fn set_flag_settings_path(path: Option<PathBuf>) {
    if let Ok(mut slot) = FLAG_SETTINGS_PATH.write() {
        *slot = path;
    }
    crate::utils::settings::settings_cache::reset_settings_cache();
}

/// Return SDK-provided inline flag settings, if present.
///
/// Maps to: CC `bootstrap/state.ts:getFlagSettingsInline`.
pub fn get_flag_settings_inline() -> Option<serde_json::Value> {
    FLAG_SETTINGS_INLINE
        .read()
        .ok()
        .and_then(|settings| settings.clone())
}

/// Set SDK-provided inline flag settings for this process.
///
/// Maps to: CC `bootstrap/state.ts:setFlagSettingsInline`.
pub fn set_flag_settings_inline(settings: Option<serde_json::Value>) {
    if let Ok(mut slot) = FLAG_SETTINGS_INLINE.write() {
        *slot = settings;
    }
    // See `set_flag_settings_path`: changing a settings SOURCE invalidates the
    // merged-settings cache.
    crate::utils::settings::settings_cache::reset_settings_cache();
}

/// Maps to: CC `bootstrap/state.ts:1158-1160` `getOauthTokenFromFd`.
pub fn get_oauth_token_from_fd() -> Option<Option<String>> {
    OAUTH_TOKEN_FROM_FD
        .read()
        .expect("OAuth FD token lock poisoned")
        .clone()
}

/// Maps to: CC `bootstrap/state.ts:1162-1164` `setOauthTokenFromFd`.
pub fn set_oauth_token_from_fd(token: Option<String>) {
    *OAUTH_TOKEN_FROM_FD
        .write()
        .expect("OAuth FD token lock poisoned") = Some(token);
}

/// Maps to: CC `bootstrap/state.ts:1166-1168` `getApiKeyFromFd`.
pub fn get_api_key_from_fd() -> Option<Option<String>> {
    API_KEY_FROM_FD
        .read()
        .expect("API-key FD lock poisoned")
        .clone()
}

/// Maps to: CC `bootstrap/state.ts:1170-1172` `setApiKeyFromFd`.
pub fn set_api_key_from_fd(key: Option<String>) {
    *API_KEY_FROM_FD.write().expect("API-key FD lock poisoned") = Some(key);
}

/// Maps to: CC `bootstrap/state.ts:919-930` `resetStateForTests` for the
/// auth-file-descriptor fields owned by this Rust state projection.
#[cfg(test)]
pub(crate) fn reset_auth_file_descriptor_caches_for_testing() {
    *OAUTH_TOKEN_FROM_FD
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    *API_KEY_FROM_FD
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

/// Return CLI-provided `--agents` JSON, if present.
///
/// Maps to: CC `main.tsx` `agentsJson = options.agents` and subsequent
/// `parseAgentsFromJson(parsedAgents, 'flagSettings')` merge.
pub fn get_cli_agents_json() -> Option<serde_json::Value> {
    CLI_AGENTS_JSON
        .read()
        .ok()
        .and_then(|agents| agents.clone())
}

/// Set parsed CLI-provided `--agents` JSON for this process.
///
/// Maps to: CC `main.tsx` parsed `agentsJson` handoff into startup agent definitions.
pub fn set_cli_agents_json(agents: Option<serde_json::Value>) {
    if let Ok(mut slot) = CLI_AGENTS_JSON.write() {
        *slot = agents;
    }
}

/// Return session-only plugin directories from `--plugin-dir`.
///
/// Maps to: CC `utils/plugins/pluginLoader.ts#setInlinePlugins/getInlinePlugins`
/// as populated by `main.tsx` pre-action parsing.
pub fn get_inline_plugins() -> Vec<PathBuf> {
    INLINE_PLUGINS
        .read()
        .map(|plugins| plugins.clone())
        .unwrap_or_default()
}

/// Set session-only plugin directories from `--plugin-dir`.
///
/// Maps to: CC `main.tsx` `setInlinePlugins(pluginDir)`.
pub fn set_inline_plugins(plugins: Vec<PathBuf>) {
    if let Ok(mut slot) = INLINE_PLUGINS.write() {
        *slot = plugins;
    }
}

/// Return whether this session should use `cowork_plugins`.
///
/// Maps to: CC `bootstrap/state.ts#getUseCoworkPlugins`.
pub fn get_use_cowork_plugins() -> bool {
    USE_COWORK_PLUGINS
        .read()
        .map(|value| *value)
        .unwrap_or(false)
}

/// Set whether this session should use `cowork_plugins`.
///
/// Maps to: CC `bootstrap/state.ts#setUseCoworkPlugins`.
pub fn set_use_cowork_plugins(value: bool) {
    if let Ok(mut slot) = USE_COWORK_PLUGINS.write() {
        *slot = value;
    }
}

/// Return the active session id.
///
/// Maps to: CC `bootstrap/state.ts:getSessionId`.
pub fn get_session_id() -> String {
    SESSION_ID
        .read()
        .map(|session_id| session_id.clone())
        .unwrap_or_else(|_| uuid::Uuid::new_v4().to_string())
}

/// Switch the active session id.
///
/// Maps to: CC `bootstrap/state.ts:switchSession(...)` for the id-only state
/// used by filesystem prompt helpers. Transcript/project-dir switching remains
/// owned by the REPL resume flow.
pub fn set_session_id(session_id: impl Into<String>) {
    switch_session(session_id, None);
}

/// Atomically switch session identity and the directory containing its JSONL.
/// Maps to: CC `bootstrap/state.ts#switchSession`.
pub fn switch_session(session_id: impl Into<String>, project_dir: Option<PathBuf>) {
    if let (Ok(mut id), Ok(mut dir)) = (SESSION_ID.write(), SESSION_PROJECT_DIR.write()) {
        *id = session_id.into();
        *dir = project_dir;
    }
}

/// Maps to: CC `bootstrap/state.ts#getSessionProjectDir`.
pub fn get_session_project_dir() -> Option<PathBuf> {
    SESSION_PROJECT_DIR
        .read()
        .ok()
        .and_then(|project_dir| project_dir.clone())
}

/// Maps to: CC `bootstrap/state.ts#regenerateSessionId`.
///
/// Drops the outgoing session's plan-slug entry and optionally records the
/// previous id as `parentSessionId` for analytics lineage.
pub fn regenerate_session_id(set_current_as_parent: bool) -> String {
    let previous = get_session_id();
    if set_current_as_parent {
        if let Ok(mut slot) = PARENT_SESSION_ID.write() {
            *slot = Some(previous.clone());
        }
    }
    crate::utils::plans::clear_plan_slug(Some(&previous));
    let next = uuid::Uuid::new_v4().to_string();
    set_session_id(next.clone());
    // No environment side effect here: CC's CLAUDE_CODE_SESSION_ID env update
    // lives only in the /clear path (`commands/clear/conversation.ts:204-207`),
    // not in `bootstrap/state.ts#regenerateSessionId`. Other callers (e.g.
    // `cli/print.ts` session forking) regenerate without touching the env.
    next
}

/// Maps to: CC `bootstrap/state.ts#getParentSessionId`.
pub fn get_parent_session_id() -> Option<String> {
    PARENT_SESSION_ID.read().ok().and_then(|slot| slot.clone())
}

/// Return teams created during this process session.
///
/// Maps to: CC `bootstrap/state.ts#getSessionCreatedTeams`.
pub fn get_session_created_teams() -> HashSet<String> {
    SESSION_CREATED_TEAMS
        .read()
        .map(|teams| teams.clone())
        .unwrap_or_default()
}

/// Maps to: CC `getSessionCreatedTeams().add(teamName)`.
pub fn add_session_created_team(team_name: impl Into<String>) {
    if let Ok(mut teams) = SESSION_CREATED_TEAMS.write() {
        teams.insert(team_name.into());
    }
}

/// Maps to: CC `getSessionCreatedTeams().delete(teamName)`.
pub fn remove_session_created_team(team_name: &str) {
    if let Ok(mut teams) = SESSION_CREATED_TEAMS.write() {
        teams.remove(team_name);
    }
}

/// Maps to: CC `getSessionCreatedTeams().clear()` and `resetStateForTests()`.
pub fn clear_session_created_teams() {
    if let Ok(mut teams) = SESSION_CREATED_TEAMS.write() {
        teams.clear();
    }
}

/// Return whether this process is currently in the interactive TUI session.
///
/// Maps to: CC `bootstrap/state.ts:getIsInteractive`.
pub fn get_is_interactive() -> bool {
    IS_INTERACTIVE
        .read()
        .map(|is_interactive| *is_interactive)
        .unwrap_or(true)
}

/// Set the interactive-session flag.
///
/// Maps to: CC `bootstrap/state.ts:setIsInteractive`.
pub fn set_is_interactive(value: bool) {
    if let Ok(mut slot) = IS_INTERACTIVE.write() {
        *slot = value;
    }
}

/// Return whether the current session is non-interactive.
///
/// Maps to: CC `bootstrap/state.ts:1057-1059` `getIsNonInteractiveSession` —
/// `!STATE.isInteractive`, nothing else. `main.tsx:1104-1120` is what puts a
/// real value in that slot, and [`crate::main::initialize_is_interactive`] is
/// the port's copy of it.
pub fn get_is_non_interactive_session() -> bool {
    !get_is_interactive() || non_interactive_env_override()
}

/// Test-only seam: `CLAUDE_CODE_NON_INTERACTIVE` / `COMETIX_NON_INTERACTIVE` /
/// `COMETIX_NON_INTERACTIVE_SESSION`.
///
/// CC has no env equivalent — `rg -c CLAUDE_CODE_NON_INTERACTIVE ../rebuild/src`
/// exits 1. These exist because the port had no production writer for
/// [`set_is_interactive`] until `main.tsx:1104-1120` was ported, so a test that
/// needed the headless branch of `getIsNonInteractiveSession()` had only an env
/// var to reach it with (`agent_tool/fork_subagent.rs#fork_veto_environment`,
/// `tools/mod.rs`, `query.rs`, `services/mcp/{config,headers_helper}.rs`).
///
/// Every write to all three names — set, unset and restore — sits after its
/// file's final top-level `#[cfg(test)]`, i.e. inside `mod tests`; no
/// production code sets them. The seam stays for those tests, and `cfg(test)`
/// keeps it out of a real run, where the argv/TTY computation is now the only
/// input, as in CC.
#[cfg(test)]
fn non_interactive_env_override() -> bool {
    // The tests write these through the carrier, so the seam reads it too.
    ["CLAUDE_CODE_NON_INTERACTIVE", "COMETIX_NON_INTERACTIVE", "COMETIX_NON_INTERACTIVE_SESSION"]
        .into_iter()
        .any(|key| crate::utils::env_utils::is_env_truthy(crate::utils::process_env::var(key).as_deref()))
}

#[cfg(not(test))]
fn non_interactive_env_override() -> bool {
    false
}

/// Restore [`IS_INTERACTIVE`] on drop, so a test that drives the startup
/// computation cannot leak the headless flag into whatever runs next.
///
/// Same bargain as `test_env::EnvVarGuard`: the caller holds `TEST_ENV_LOCK`
/// to serialise the mutation, this undoes it even through a panic.
#[cfg(test)]
pub struct IsInteractiveGuard {
    previous: bool,
}

#[cfg(test)]
impl IsInteractiveGuard {
    /// Capture the current value; the caller mutates afterwards.
    pub fn capture() -> Self {
        Self {
            previous: get_is_interactive(),
        }
    }
}

#[cfg(test)]
impl Drop for IsInteractiveGuard {
    fn drop(&mut self) {
        set_is_interactive(self.previous);
    }
}

/// Maps to: CC `bootstrap/state.ts:1234-1237`
/// `preferThirdPartyAuthentication`.
pub fn prefer_third_party_authentication() -> bool {
    get_is_non_interactive_session()
        && crate::utils::process_env::var("CLAUDE_CODE_ENTRYPOINT").as_deref() != Some("claude-vscode")
}

/// Maps to CC `getKairosActive()`.
pub fn get_kairos_active() -> bool {
    KAIROS_ACTIVE.read().map(|active| *active).unwrap_or(false)
}

/// Maps to CC `setKairosActive(value)`.
pub fn set_kairos_active(value: bool) {
    if let Ok(mut active) = KAIROS_ACTIVE.write() {
        *active = value;
    }
}

/// Maps to CC `getUserMsgOptIn()`.
pub fn get_user_msg_opt_in() -> bool {
    USER_MSG_OPT_IN
        .read()
        .map(|opted_in| *opted_in)
        .unwrap_or(false)
}

/// Maps to CC `setUserMsgOptIn(value)`.
pub fn set_user_msg_opt_in(value: bool) {
    if let Ok(mut opted_in) = USER_MSG_OPT_IN.write() {
        *opted_in = value;
    }
}

/// Return the startup working directory.
///
/// Maps to: CC `bootstrap/state.ts:getOriginalCwd`.
pub fn get_original_cwd() -> PathBuf {
    ORIGINAL_CWD
        .read()
        .map(|cwd| cwd.clone())
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

/// Override the startup working directory.
///
/// Maps to: CC `bootstrap/state.ts:setOriginalCwd`.
pub fn set_original_cwd(cwd: impl Into<PathBuf>) {
    if let Ok(mut slot) = ORIGINAL_CWD.write() {
        // CC bootstrap/state.ts:516: preserve spelling except Unicode NFC.
        *slot = PathBuf::from(cwd.into().to_string_lossy().nfc().collect::<String>());
    }
}

/// Maps to: CC `bootstrap/state.ts` `hasExitedPlanModeInSession()`.
pub fn has_exited_plan_mode_in_session() -> bool {
    HAS_EXITED_PLAN_MODE.read().map(|v| *v).unwrap_or(false)
}

/// Maps to: CC `bootstrap/state.ts` `setHasExitedPlanMode(...)`.
pub fn set_has_exited_plan_mode(value: bool) {
    if let Ok(mut slot) = HAS_EXITED_PLAN_MODE.write() {
        *slot = value;
    }
}

/// Maps to: CC `bootstrap/state.ts` `needsPlanModeExitAttachment()`.
pub fn needs_plan_mode_exit_attachment() -> bool {
    NEEDS_PLAN_MODE_EXIT_ATTACHMENT
        .read()
        .map(|v| *v)
        .unwrap_or(false)
}

/// Maps to: CC `bootstrap/state.ts` `setNeedsPlanModeExitAttachment(...)`.
pub fn set_needs_plan_mode_exit_attachment(value: bool) {
    if let Ok(mut slot) = NEEDS_PLAN_MODE_EXIT_ATTACHMENT.write() {
        *slot = value;
    }
}

/// Maps to: CC `bootstrap/state.ts` `handlePlanModeTransition(from, to)`.
///
/// Clears pending plan_mode_exit attachment when entering plan; sets it when
/// leaving plan so the next turn can inject the exit attachment.
pub fn handle_plan_mode_transition(from_mode: &str, to_mode: &str) {
    if to_mode == "plan" && from_mode != "plan" {
        set_needs_plan_mode_exit_attachment(false);
    }
    if from_mode == "plan" && to_mode != "plan" {
        set_needs_plan_mode_exit_attachment(true);
    }
}

/// Maps to: CC `bootstrap/state.ts` `needsAutoModeExitAttachment()`.
pub fn needs_auto_mode_exit_attachment() -> bool {
    NEEDS_AUTO_MODE_EXIT_ATTACHMENT
        .read()
        .map(|v| *v)
        .unwrap_or(false)
}

/// Maps to: CC `bootstrap/state.ts` `setNeedsAutoModeExitAttachment(...)`.
pub fn set_needs_auto_mode_exit_attachment(value: bool) {
    if let Ok(mut slot) = NEEDS_AUTO_MODE_EXIT_ATTACHMENT.write() {
        *slot = value;
    }
}

/// Maps to: CC `bootstrap/state.ts` `handleAutoModeTransition(from, to)`.
/// Skips auto↔plan (handled by prepareContextForPlanMode / ExitPlanMode).
pub fn handle_auto_mode_transition(from_mode: &str, to_mode: &str) {
    if (from_mode == "auto" && to_mode == "plan") || (from_mode == "plan" && to_mode == "auto") {
        return;
    }
    let from_is_auto = from_mode == "auto";
    let to_is_auto = to_mode == "auto";
    if to_is_auto && !from_is_auto {
        set_needs_auto_mode_exit_attachment(false);
    }
    if from_is_auto && !to_is_auto {
        set_needs_auto_mode_exit_attachment(true);
    }
}

/// Return additional directories from the explicit `--add-dir` flag.
///
/// Maps to: CC `bootstrap/state.ts:getAdditionalDirectoriesForClaudeMd`.
pub fn get_additional_directories_for_claude_md() -> Vec<PathBuf> {
    ADDITIONAL_DIRECTORIES_FOR_CLAUDE_MD
        .read()
        .map(|directories| directories.clone())
        .unwrap_or_default()
}

/// Set additional directories from the explicit `--add-dir` flag.
///
/// Maps to: CC `bootstrap/state.ts:setAdditionalDirectoriesForClaudeMd`.
pub fn set_additional_directories_for_claude_md(directories: Vec<PathBuf>) {
    if let Ok(mut slot) = ADDITIONAL_DIRECTORIES_FOR_CLAUDE_MD.write() {
        *slot = directories;
    }
}

/// Maps to CC `bootstrap/state.ts#getAllowedSettingSources`.
pub fn get_allowed_setting_sources() -> Vec<String> {
    ALLOWED_SETTING_SOURCES
        .read()
        .map(|sources| sources.clone())
        .unwrap_or_default()
}

/// Maps to CC `bootstrap/state.ts#setAllowedSettingSources`.
pub fn set_allowed_setting_sources(sources: Vec<String>) {
    if let Ok(mut slot) = ALLOWED_SETTING_SOURCES.write() {
        *slot = sources;
    }
    // Which sources participate in the merge is an input to the cached result.
    crate::utils::settings::settings_cache::reset_settings_cache();
}

/// Maps to CC `bootstrap/state.ts#getAgentColorMap`.
pub fn get_agent_color_map()
-> &'static RwLock<HashMap<String, crate::tools::agent_tool::agent_color_manager::AgentColorName>> {
    &AGENT_COLOR_MAP
}

/// Return the channel allowlist parsed from `--channels` and accepted dev
/// channels.
///
/// Maps to: CC `bootstrap/state.ts:getAllowedChannels`.
pub fn get_allowed_channels() -> Vec<ChannelEntry> {
    ALLOWED_CHANNELS
        .read()
        .map(|channels| channels.clone())
        .unwrap_or_default()
}

/// Set the channel allowlist parsed from `--channels`.
///
/// Maps to: CC `bootstrap/state.ts:setAllowedChannels`.
pub fn set_allowed_channels(entries: Vec<ChannelEntry>) {
    if let Ok(mut slot) = ALLOWED_CHANNELS.write() {
        *slot = entries;
    }
}

/// Maps to: CC `bootstrap/state.ts:82` `'markdown' | 'html' | undefined`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuestionPreviewFormat {
    Markdown,
    Html,
}

/// Maps to: CC `bootstrap/state.ts:1120-1122` `getQuestionPreviewFormat`.
pub fn get_question_preview_format() -> Option<QuestionPreviewFormat> {
    QUESTION_PREVIEW_FORMAT
        .read()
        .ok()
        .and_then(|format| *format)
}

/// Maps to: CC `bootstrap/state.ts:1124-1126` `setQuestionPreviewFormat`.
pub fn set_question_preview_format(format: Option<QuestionPreviewFormat>) {
    if let Ok(mut slot) = QUESTION_PREVIEW_FORMAT.write() {
        *slot = format;
    }
}

/// Return whether any session channel came from
/// `--dangerously-load-development-channels`.
///
/// Maps to: CC `bootstrap/state.ts:getHasDevChannels`.
pub fn get_has_dev_channels() -> bool {
    HAS_DEV_CHANNELS
        .read()
        .map(|has_dev| *has_dev)
        .unwrap_or(false)
}

/// Set the session-wide dev-channel flag used only for notice copy.
///
/// Maps to: CC `bootstrap/state.ts:setHasDevChannels`.
pub fn set_has_dev_channels(value: bool) {
    if let Ok(mut slot) = HAS_DEV_CHANNELS.write() {
        *slot = value;
    }
}

// Registered-hook operations join the established cross-thread synchronous
// store turn. The loader/pruner hold an outer turn across source clear/register
// pairs; readers and independent callback writers cannot observe a half-swap.
// Lock order is always StoreTurn -> REGISTERED_HOOKS, with same-thread reentry.
/// Atomically replace plugin-contributed registered hooks.
///
/// Maps to: CC `bootstrap/state.ts::{clearRegisteredHooks,registerHooks}` as
/// the single observable swap used by `loadPluginHooks()`.
pub fn replace_registered_hooks(config: crate::schemas::hooks::RegisteredHooks) {
    let _turn = crate::state::store::enter_store_turn_segment();
    if let Ok(mut registered) = REGISTERED_HOOKS.write() {
        *registered = (!config.is_empty()).then_some(config);
    }
}

/// Maps to: CC `bootstrap/state.ts:1419-1434` `registerHookCallbacks` — may be
/// called multiple times, so matchers MERGE per event (never overwrite).
pub fn register_hook_callbacks(
    hooks: std::collections::HashMap<String, Vec<crate::schemas::hooks::RegisteredHookMatcher>>,
) {
    let _turn = crate::state::store::enter_store_turn_segment();
    if let Ok(mut registered) = REGISTERED_HOOKS.write() {
        let slot = registered.get_or_insert_with(Default::default);
        for (event, matchers) in hooks {
            slot.entry(event).or_default().extend(matchers);
        }
    }
}

/// Maps to: CC `bootstrap/state.ts#getRegisteredHooks`.
pub fn get_registered_hooks() -> Option<crate::schemas::hooks::RegisteredHooks> {
    let _turn = crate::state::store::enter_store_turn_segment();
    REGISTERED_HOOKS
        .read()
        .ok()
        .and_then(|registered| registered.clone())
}

/// Maps to: CC `bootstrap/state.ts#clearRegisteredHooks`.
pub fn clear_registered_hooks() {
    let _turn = crate::state::store::enter_store_turn_segment();
    if let Ok(mut registered) = REGISTERED_HOOKS.write() {
        *registered = None;
    }
}

/// Maps to: CC `bootstrap/state.ts:1446-1462#clearRegisteredPluginHooks`.
pub fn clear_registered_plugin_hooks() {
    let _turn = crate::state::store::enter_store_turn_segment();
    if let Ok(mut slot) = REGISTERED_HOOKS.write() {
        if let Some(current) = slot.as_mut() {
            current.retain(|_, matchers| {
                matchers.retain(|matcher| matcher.plugin_root.is_none());
                !matchers.is_empty()
            });
            if current.is_empty() {
                *slot = None;
            }
        }
    }
}

/// Maps to: CC `bootstrap/state.ts#setSessionTrustAccepted`.
pub fn set_session_trust_accepted(accepted: bool) {
    if let Ok(mut slot) = SESSION_TRUST_ACCEPTED.write() {
        *slot = accepted;
    }
}

/// Maps to: CC `bootstrap/state.ts#getSessionTrustAccepted`.
pub fn get_session_trust_accepted() -> bool {
    SESSION_TRUST_ACCEPTED
        .read()
        .map(|accepted| *accepted)
        .unwrap_or(false)
}

/// Maps to: CC `bootstrap/state.ts#setSessionPersistenceDisabled`.
pub fn set_session_persistence_disabled(disabled: bool) {
    if let Ok(mut slot) = SESSION_PERSISTENCE_DISABLED.write() {
        *slot = disabled;
    }
}

/// Maps to: CC `bootstrap/state.ts#isSessionPersistenceDisabled`.
pub fn is_session_persistence_disabled() -> bool {
    SESSION_PERSISTENCE_DISABLED
        .read()
        .map(|disabled| *disabled)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn channel_state_matches_official_getters_and_setters() {
        super::set_allowed_channels(vec![
            super::ChannelEntry::plugin("mailbox", "anthropic", false),
            super::ChannelEntry::server("planner", true),
        ]);
        super::set_has_dev_channels(true);

        assert_eq!(super::get_allowed_channels().len(), 2);
        assert!(super::get_has_dev_channels());

        super::set_allowed_channels(Vec::new());
        super::set_has_dev_channels(false);
        assert!(super::get_allowed_channels().is_empty());
        assert!(!super::get_has_dev_channels());
    }

    #[test]
    fn session_persistence_disabled_matches_official_state_getter_and_setter() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        super::set_session_persistence_disabled(true);
        assert!(super::is_session_persistence_disabled());
        super::set_session_persistence_disabled(false);
        assert!(!super::is_session_persistence_disabled());
    }

    #[test]
    fn switch_session_and_regenerate_keep_project_directory_atomic() {
        // Session id/project dir are process-global; racing another test that
        // reads or regenerates them (e.g. clear_conversation) flakes both.
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let previous_id = super::get_session_id();
        let previous_dir = super::get_session_project_dir();
        super::switch_session(
            "resumed-session",
            Some(std::path::PathBuf::from("/projects/resumed")),
        );
        assert_eq!(super::get_session_id(), "resumed-session");
        assert_eq!(
            super::get_session_project_dir(),
            Some(std::path::PathBuf::from("/projects/resumed"))
        );

        let regenerated = super::regenerate_session_id(false);
        assert_eq!(super::get_session_id(), regenerated);
        assert_eq!(super::get_session_project_dir(), None);
        super::switch_session(previous_id, previous_dir);
    }

    #[test]
    fn non_interactive_session_mirrors_official_inverse_interactive_state_and_env_override() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::remove("CLAUDE_CODE_NON_INTERACTIVE");
        crate::utils::process_env::remove("COMETIX_NON_INTERACTIVE");
        crate::utils::process_env::remove("COMETIX_NON_INTERACTIVE_SESSION");

        super::set_is_interactive(true);
        assert!(super::get_is_interactive());
        assert!(!super::get_is_non_interactive_session());

        super::set_is_interactive(false);
        assert!(!super::get_is_interactive());
        assert!(super::get_is_non_interactive_session());

        super::set_is_interactive(true);
        crate::utils::process_env::set("CLAUDE_CODE_NON_INTERACTIVE", "1");
        assert!(super::get_is_non_interactive_session());

        crate::utils::process_env::remove("CLAUDE_CODE_NON_INTERACTIVE");
        super::set_is_interactive(true);
    }
}
