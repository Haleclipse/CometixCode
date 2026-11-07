//! Maps to: CC `tools/ConfigTool/supportedSettings.ts` (the whole file).
//!
//! The four constant lists at the top are CC's imports from
//! `utils/theme.ts` (`THEME_NAMES`/`THEME_SETTINGS`) and
//! `utils/configConstants.ts` (`EDITOR_MODES`, `NOTIFICATION_CHANNELS`,
//! `TEAMMATE_MODES`). Neither owner file is ported yet and this is their only
//! consumer, so they live with the single reader until those modules land.

/// Maps to: CC `utils/theme.ts:91-98` `THEME_NAMES`. `THEME_SETTINGS` prepends
/// `"auto"` behind `feature('AUTO_THEME')`, which external builds do not carry.
const THEME_NAMES: &[&str] = &[
    "dark",
    "light",
    "light-daltonized",
    "dark-daltonized",
    "light-ansi",
    "dark-ansi",
];

/// Maps to: CC `utils/configConstants.ts:15` `EDITOR_MODES`.
const EDITOR_MODES: &[&str] = &["normal", "vim"];

/// Maps to: CC `utils/configConstants.ts:4-12` `NOTIFICATION_CHANNELS`.
const NOTIFICATION_CHANNELS: &[&str] = &[
    "auto",
    "iterm2",
    "iterm2_with_bell",
    "terminal_bell",
    "kitty",
    "ghostty",
    "notifications_disabled",
];

/// Maps to: CC `utils/configConstants.ts:21` `TEAMMATE_MODES`.
const TEAMMATE_MODES: &[&str] = &["auto", "tmux", "in-process"];

/// Maps to: CC `supportedSettings.ts:16` `SettingConfig['source']`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingSource {
    Global,
    Settings,
}

/// Maps to: CC `supportedSettings.ts:17` `SettingConfig['type']`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingType {
    Boolean,
    String,
}

/// Maps to: CC `supportedSettings.ts:13` `SyncableAppStateKey`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncableAppStateKey {
    Verbose,
}

/// Maps to: CC `supportedSettings.ts:23-24`
/// `validateOnWrite?: (v: unknown) => Promise<{ valid: boolean; error?: string }>`.
///
/// A plain fn pointer, like the `getOptions` / `formatOnRead` slots: the source
/// arrows capture nothing, so a non-capturing `fn` is their exact shape and the
/// registry stays `Copy`. The `Promise` is the boxed future — a fn pointer needs
/// one nameable return type and `async fn` futures are anonymous (`dyn AsyncFn`
/// is not dyn-compatible). `{ valid: true }` / `{ valid: false, error }`
/// collapse onto `Result<(), String>`; every invalid branch in
/// `validateModel.ts` sets `error`. `voiceEnabled` / `remoteControlAtStartup`
/// do not belong here — they are `call()` special cases in the source, not
/// `validateOnWrite` entries.
pub type ValidateOnWrite =
    for<'a> fn(&'a serde_json::Value) -> futures::future::BoxFuture<'a, Result<(), String>>;

/// Maps to: CC `supportedSettings.ts:15-27` `SettingConfig`.
///
/// All three CC function fields (`getOptions`, `validateOnWrite`,
/// `formatOnRead`) are fn-pointer slots declared next to their entry, as in the
/// source. Only `model` fills `validateOnWrite` today (`:104`); `config_output`
/// awaits whatever the slot holds (`ConfigTool.ts:216-218`), so a later setting
/// hangs its owner on its own entry without another `if setting ==` in the
/// write path.
#[derive(Clone, Copy, Debug)]
pub struct SettingConfig {
    pub key: &'static str,
    pub source: SettingSource,
    pub setting_type: SettingType,
    pub description: &'static str,
    /// CC `path` — overrides the dotted split of `key`.
    pub path: Option<&'static [&'static str]>,
    /// CC `options` — a static allowlist.
    pub options: Option<&'static [&'static str]>,
    /// CC `getOptions` — resolved at prompt/validation time.
    pub get_options: Option<fn() -> Vec<String>>,
    pub app_state_key: Option<SyncableAppStateKey>,
    /// CC `validateOnWrite` — async validation before a write lands.
    pub validate_on_write: Option<ValidateOnWrite>,
    /// CC `formatOnRead` — display formatting for get operations.
    ///
    /// Both sides are `Option` because CC's `getValue` returns JS `undefined`
    /// for an unset key and `formatOnRead` is handed that `undefined`
    /// unchanged: `None` is `undefined`, `Some(Value::Null)` is JSON `null`.
    /// `model`'s formatter (`:105`) is exactly the reader that separates them.
    pub format_on_read: Option<fn(Option<&serde_json::Value>) -> Option<serde_json::Value>>,
}

impl SettingConfig {
    const fn new(
        key: &'static str,
        source: SettingSource,
        setting_type: SettingType,
        description: &'static str,
    ) -> Self {
        Self {
            key,
            source,
            setting_type,
            description,
            path: None,
            options: None,
            get_options: None,
            app_state_key: None,
            validate_on_write: None,
            format_on_read: None,
        }
    }

    const fn options(mut self, options: &'static [&'static str]) -> Self {
        self.options = Some(options);
        self
    }

    const fn get_options(mut self, get_options: fn() -> Vec<String>) -> Self {
        self.get_options = Some(get_options);
        self
    }

    const fn app_state_key(mut self, app_state_key: SyncableAppStateKey) -> Self {
        self.app_state_key = Some(app_state_key);
        self
    }

    const fn validate_on_write(mut self, validate_on_write: ValidateOnWrite) -> Self {
        self.validate_on_write = Some(validate_on_write);
        self
    }

    const fn format_on_read(
        mut self,
        format_on_read: fn(Option<&serde_json::Value>) -> Option<serde_json::Value>,
    ) -> Self {
        self.format_on_read = Some(format_on_read);
        self
    }
}

/// Maps to: CC `supportedSettings.ts:160`
/// `formatOnRead: () => getRemoteControlAtStartup()`. The arrow takes no
/// parameter, so the stored value — `undefined` included — never reaches it.
pub(super) fn format_remote_control_at_startup_on_read(
    _value: Option<&serde_json::Value>,
) -> Option<serde_json::Value> {
    Some(serde_json::Value::Bool(
        crate::utils::config::get_remote_control_at_startup(),
    ))
}

/// Maps to: CC `supportedSettings.ts:29-186` `SUPPORTED_SETTINGS`, including the
/// build/user-type gated entries.
pub fn supported_settings() -> Vec<SettingConfig> {
    use SettingSource::{Global, Settings};
    use SettingType::{Boolean, String as Str};

    let mut settings = vec![
        SettingConfig::new("theme", Global, Str, "Color theme for the UI").options(THEME_NAMES),
        SettingConfig::new("editorMode", Global, Str, "Key binding mode").options(EDITOR_MODES),
        SettingConfig::new("verbose", Global, Boolean, "Show detailed debug output")
            .app_state_key(SyncableAppStateKey::Verbose),
        SettingConfig::new(
            "preferredNotifChannel",
            Global,
            Str,
            "Preferred notification channel",
        )
        .options(NOTIFICATION_CHANNELS),
        SettingConfig::new(
            "autoCompactEnabled",
            Global,
            Boolean,
            "Auto-compact when context is full",
        ),
        SettingConfig::new("autoMemoryEnabled", Settings, Boolean, "Enable auto-memory"),
        SettingConfig::new(
            "autoDreamEnabled",
            Settings,
            Boolean,
            "Enable background memory consolidation",
        ),
        SettingConfig::new(
            "fileCheckpointingEnabled",
            Global,
            Boolean,
            "Enable file checkpointing for code rewind",
        ),
        SettingConfig::new(
            "showTurnDuration",
            Global,
            Boolean,
            "Show turn duration message after responses (e.g., \"Cooked for 1m 6s\")",
        ),
        SettingConfig::new(
            "terminalProgressBarEnabled",
            Global,
            Boolean,
            "Show OSC 9;4 progress indicator in supported terminals",
        ),
        SettingConfig::new(
            "todoFeatureEnabled",
            Global,
            Boolean,
            "Enable todo/task tracking",
        ),
        SettingConfig::new(
            "alwaysThinkingEnabled",
            Settings,
            Boolean,
            "Enable extended thinking (false to disable)",
        ),
        SettingConfig::new(
            "permissions.defaultMode",
            Settings,
            Str,
            "Default permission mode for tool usage",
        )
        .options(permission_default_modes()),
        SettingConfig::new(
            "language",
            Settings,
            Str,
            "Preferred language for Claude responses and voice dictation (e.g., \"japanese\", \"spanish\")",
        ),
        SettingConfig::new(
            "teammateMode",
            Global,
            Str,
            "How to spawn teammates: \"tmux\" for traditional tmux, \"in-process\" for same process, \"auto\" to choose automatically",
        )
        .options(TEAMMATE_MODES),
    ];

    // Maps to CC `supportedSettings.ts:134-143` `process.env.USER_TYPE === 'ant'`.
    if crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::ManagedConfiguration,
    ) {
        settings.push(SettingConfig::new(
            "classifierPermissionsEnabled",
            Settings,
            Boolean,
            "Enable AI-based classification for Bash(prompt:...) permission rules",
        ));
    }
    // Maps to CC `supportedSettings.ts:144-152` `feature('VOICE_MODE')`.
    if cfg!(feature = "voice_mode") {
        settings.push(SettingConfig::new(
            "voiceEnabled",
            Settings,
            Boolean,
            "Enable voice dictation (hold-to-talk)",
        ));
    }
    // Maps to CC `supportedSettings.ts:153-163` `feature('BRIDGE_MODE')`.
    if crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::BridgeMode,
    ) {
        settings.push(
            SettingConfig::new(
                "remoteControlAtStartup",
                Global,
                Boolean,
                "Enable Remote Control for all sessions (true | false | default)",
            )
            .format_on_read(format_remote_control_at_startup_on_read),
        );
    }
    // Maps to CC `supportedSettings.ts:164-185`
    // `feature('KAIROS') || feature('KAIROS_PUSH_NOTIFICATION')`.
    if crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::Kairos,
    ) || crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::KairosPushNotification,
    ) {
        settings.extend([
            SettingConfig::new(
                "taskCompleteNotifEnabled",
                Global,
                Boolean,
                "Push to your mobile device when idle after Claude finishes (requires Remote Control)",
            ),
            SettingConfig::new(
                "inputNeededNotifEnabled",
                Global,
                Boolean,
                "Push to your mobile device when a permission prompt or question is waiting (requires Remote Control)",
            ),
            SettingConfig::new(
                "agentPushNotifEnabled",
                Global,
                Boolean,
                "Allow Claude to push to your mobile device when it deems it appropriate (requires Remote Control)",
            ),
        ]);
    }
    settings
}

/// Maps to: CC `supportedSettings.ts:117-119` — `feature('TRANSCRIPT_CLASSIFIER')`
/// adds `auto`.
fn permission_default_modes() -> &'static [&'static str] {
    const WITH_AUTO: &[&str] = &["default", "plan", "acceptEdits", "dontAsk", "auto"];
    const WITHOUT_AUTO: &[&str] = &["default", "plan", "acceptEdits", "dontAsk"];
    if crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::TranscriptClassifier,
    ) {
        WITH_AUTO
    } else {
        WITHOUT_AUTO
    }
}

/// Maps to: CC `supportedSettings.ts:192-194` `getConfig`.
pub fn get_config(key: &str) -> Option<SettingConfig> {
    supported_settings()
        .into_iter()
        .find(|setting| setting.key == key)
}

/// Maps to: CC `supportedSettings.ts:188-190` `isSupported`.
///
/// PRESERVE — zero Rust callers, deliberately. Its CC consumer IS ported:
/// `ConfigTool.ts:126` runs `if (!isSupported(setting)) return { error: Unknown
/// setting… }` and then `getConfig(setting)!` on the next line, and
/// `config_tool/mod.rs#config_output` collapses that pair into the single
/// `get_config(setting)` `Option` the `!` non-null assertion says is safe. So
/// the behaviour is present, only the two-lookup shape is not; the symbol
/// stays because CC exports it and a later reader (or a re-split of
/// `config_output`) needs the name to exist. Do NOT delete it under
/// "nothing calls it in Rust" — the deletion criterion is a missing CC
/// declaration, not a missing Rust caller.
pub fn is_supported(key: &str) -> bool {
    get_config(key).is_some()
}

// CC `supportedSettings.ts:196-198` `getAllKeys` has zero call sites in the
// source tree (verified: `grep -rn getAllKeys rebuild/src` matches only the
// declaration), so there is nothing for a reader here to be missing.

/// Maps to: CC `supportedSettings.ts:200-206` `getOptionsForSetting`.
pub fn get_options_for_setting(key: &str) -> Option<Vec<String>> {
    let config = get_config(key)?;
    if let Some(options) = config.options {
        return Some(options.iter().map(|option| option.to_string()).collect());
    }
    config.get_options.map(|get_options| get_options())
}

/// Maps to: CC `supportedSettings.ts:208-211` `getPath`.
pub fn get_path(key: &str) -> Vec<String> {
    match get_config(key).and_then(|config| config.path) {
        Some(path) => path.iter().map(|segment| segment.to_string()).collect(),
        None => key.split('.').map(str::to_string).collect(),
    }
}
