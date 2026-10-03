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
    MainLoopModel,
    ThinkingEnabled,
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

/// Maps to: CC `supportedSettings.ts:95-103` `model.getOptions` — the picker
/// list minus its `value: null` default entry, falling back to the source's
/// `catch` branch (`:100-102`).
///
/// The fallback is reachable only through an unwind: every read in the
/// `get_model_options()` call graph is infallible here, unlike the source's
/// `getSettings_DEPRECATED`/`getModelStrings`/`getGlobalConfig`, which throw.
pub(super) fn model_option_values() -> Vec<String> {
    std::panic::catch_unwind(|| {
        crate::utils::model::model_options::get_model_options(false)
            .into_iter()
            .filter_map(|option| option.value)
            .collect::<Vec<_>>()
    })
    .unwrap_or_else(|_| {
        MODEL_OPTIONS_FALLBACK
            .iter()
            .map(|option| (*option).to_string())
            .collect()
    })
}

/// Maps to: CC `supportedSettings.ts:101` — the `catch` branch's list.
pub(super) const MODEL_OPTIONS_FALLBACK: &[&str] = &["sonnet", "opus", "haiku"];

/// Maps to: CC `supportedSettings.ts:104` `validateOnWrite: v => validateModel(String(v))`
/// → `utils/model/validateModel.ts`.
///
/// `String(v)` runs synchronously when the arrow is invoked and the promise
/// handed back is `validateModel`'s own; same order here — coerce first, then
/// the owner's future takes the owned string. Every tier, including the live
/// `sideQuery` probe (`validateModel.ts:56-81`), is the owner's.
fn validate_model_on_write(
    value: &serde_json::Value,
) -> futures::future::BoxFuture<'_, Result<(), String>> {
    let model = super::setting_value_as_string(value);
    Box::pin(async move { crate::utils::model::validate_model::validate_model(&model).await })
}

/// Maps to: CC `supportedSettings.ts:105` `formatOnRead: v => (v === null ? 'default' : v)`.
///
/// `undefined` is NOT `null` in JS, so an unset `model` — the common case,
/// where `getValue` walked off the object and returned `undefined` — falls
/// through this formatter untouched and reads back as `undefined`, not
/// `'default'`. Only a settings file that literally stores `"model": null`
/// takes the `'default'` branch.
pub(super) fn format_model_on_read(value: Option<&serde_json::Value>) -> Option<serde_json::Value> {
    match value {
        Some(serde_json::Value::Null) => Some(serde_json::Value::String("default".to_string())),
        other => other.cloned(),
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
        SettingConfig::new("model", Settings, Str, "Override the default model")
            .app_state_key(SyncableAppStateKey::MainLoopModel)
            .get_options(model_option_values)
            .validate_on_write(validate_model_on_write)
            .format_on_read(format_model_on_read),
        SettingConfig::new(
            "alwaysThinkingEnabled",
            Settings,
            Boolean,
            "Enable extended thinking (false to disable)",
        )
        .app_state_key(SyncableAppStateKey::ThinkingEnabled),
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

#[cfg(test)]
mod tests {
    use super::super::prompt::tests::ModelSectionFixture;
    use crate::utils::test_env::TEST_ENV_LOCK;

    /// Maps to: CC `supportedSettings.ts:105` — `v === null ? 'default' : v`.
    /// `undefined` is not `null`, so the unset case passes through.
    #[test]
    fn model_format_on_read_separates_undefined_from_null() {
        assert_eq!(super::format_model_on_read(None), None);
        assert_eq!(
            super::format_model_on_read(Some(&serde_json::Value::Null)),
            Some(serde_json::json!("default"))
        );
        assert_eq!(
            super::format_model_on_read(Some(&serde_json::json!("opus"))),
            Some(serde_json::json!("opus"))
        );
    }

    /// Maps to: CC `supportedSettings.ts:104` — only `model` hangs
    /// `validateOnWrite`. `if (config.validateOnWrite)` is the only question
    /// the source asks of the slot, so the check is set-ness, not identity.
    #[test]
    fn only_model_declares_the_validate_on_write_slot() {
        assert!(
            super::get_config("model")
                .unwrap()
                .validate_on_write
                .is_some()
        );
        for key in [
            "theme",
            "language",
            "verbose",
            "alwaysThinkingEnabled",
            "editorMode",
        ] {
            assert!(
                super::get_config(key).unwrap().validate_on_write.is_none(),
                "{key}"
            );
        }
    }

    /// Maps to: CC `supportedSettings.ts:104` `v => validateModel(String(v))` —
    /// the slot's own two steps: `String(v)`, then the owner. The owner's tiers
    /// are `validate_model.rs`'s to test; the empty guard (`validateModel.ts:26-28`)
    /// and the `availableModels` allowlist (`:31-36`) only prove the call landed,
    /// and the allowlist error echoing the normalized name is what makes the
    /// coercion observable without the live probe.
    #[tokio::test]
    async fn model_slot_coerces_string_v_and_reaches_validate_model() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let fixture = ModelSectionFixture::new();
        fixture.write_settings(r#"{"availableModels":["haiku"]}"#);

        assert!(
            super::validate_model_on_write(&serde_json::json!("haiku"))
                .await
                .is_ok()
        );
        assert_eq!(
            super::validate_model_on_write(&serde_json::json!("  "))
                .await
                .unwrap_err(),
            "Model name cannot be empty"
        );
        assert_eq!(
            super::validate_model_on_write(&serde_json::json!("opus"))
                .await
                .unwrap_err(),
            "Model 'opus' is not in the list of available models"
        );
        // The input schema admits booleans and numbers for a string-typed
        // setting (`ConfigTool.ts:43-45`); `String(true)` is `"true"` and
        // `String(1)` is `"1"`.
        assert_eq!(
            super::validate_model_on_write(&serde_json::json!(true))
                .await
                .unwrap_err(),
            "Model 'true' is not in the list of available models"
        );
        assert_eq!(
            super::validate_model_on_write(&serde_json::json!(1.0))
                .await
                .unwrap_err(),
            "Model '1' is not in the list of available models"
        );
    }

    /// Maps to: CC `utils/model/aliases.ts:1-9`.
    #[test]
    fn model_aliases_match_the_official_list() {
        assert_eq!(
            crate::utils::model::aliases::MODEL_ALIASES,
            [
                "sonnet",
                "opus",
                "haiku",
                "best",
                "sonnet[1m]",
                "opus[1m]",
                "opusplan"
            ]
        );
    }
}
