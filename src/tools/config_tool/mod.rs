//! Maps to: CC `tools/ConfigTool/ConfigTool.ts` (the whole file).
//!
//! The sibling CC files each own their own Rust file: `constants.ts` →
//! [`constants`], `prompt.ts` → [`prompt`], `supportedSettings.ts` →
//! [`supported_settings`], `UI.tsx` → [`ui`].

pub mod constants;
pub mod prompt;
pub mod supported_settings;
pub mod ui;

/// Maps to: CC `tools.ts` internal-distribution `[ConfigTool]` gate.
pub fn is_config_tool_enabled() -> bool {
    crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::Tools,
    )
}

/// Maps to: CC `ConfigTool` metadata.
/// Maps to: CC `ConfigTool.ts:36-48` `inputSchema`.
///
/// `value` is a `z.union`, which zod projects as `anyOf` — not the
/// `"type": ["string", "boolean", "number"]` shorthand the hand-written literal
/// used.
pub fn input_schema() -> &'static crate::utils::zod::Schema {
    static SCHEMA: std::sync::OnceLock<crate::utils::zod::Schema> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        use crate::utils::zod;
        zod::strict_object(vec![
            (
                "setting",
                zod::string().describe(
                    "The setting key (e.g., \"theme\", \"model\", \"permissions.defaultMode\")",
                ),
            ),
            (
                "value",
                zod::union(vec![zod::string(), zod::boolean(), zod::number()])
                    .optional()
                    .describe("The new value. Omit to get current value."),
            ),
        ])
    })
}

pub fn config_tool_schema() -> crate::types::tools::Tool {
    crate::types::tools::Tool {
        name: constants::CONFIG_TOOL_NAME.to_string(),
        description: prompt::generate_prompt(),
        input_schema: crate::utils::zod_to_json_schema::zod_to_json_schema(input_schema()),
        ..Default::default()
    }
}

/// CC `tools/ConfigTool/ConfigTool.ts` `export type Output` (:65) —
/// outputSchema (:51-61).
///
/// Every `Option` here is CC's `.optional()`, i.e. JS `undefined` — NOT JSON
/// `null`. That distinction is load-bearing on the read path: `getValue`
/// (`:436-453`) returns `undefined` for an unset key, `JSON.stringify` drops an
/// `undefined` property entirely, and `${JSON.stringify(undefined)}` in a
/// template interpolates the literal text `undefined`. So `value: None` means
/// "the model sees `theme = undefined` and the wire shape has no `value` key",
/// while `value: Some(Value::Null)` means "the model sees `theme = null`".
/// [`read_config_setting_value`] is the producer that keeps them apart.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Output {
    pub(crate) success: bool,
    pub(crate) operation: Option<String>,
    pub(crate) setting: Option<String>,
    pub(crate) value: Option<serde_json::Value>,
    pub(crate) previous_value: Option<serde_json::Value>,
    pub(crate) new_value: Option<serde_json::Value>,
    pub(crate) error: Option<String>,
}

/// CC treats a missing `value` as the get operation (`input.value === undefined`).
fn is_set_operation(args: &serde_json::Value) -> bool {
    args.get("value").is_some_and(|value| !value.is_null())
}

fn config_error(operation: Option<&str>, setting: Option<&str>, error: String) -> Output {
    Output {
        success: false,
        operation: operation.map(str::to_string),
        setting: setting.map(str::to_string),
        value: None,
        previous_value: None,
        new_value: None,
        error: Some(error),
    }
}

/// Maps to: CC `tools/ConfigTool/ConfigTool.ts:239-250` — the only voice
/// pre-flight whose inputs are ported. Both branches return a bare
/// `{success, error}` at the source, without `operation`/`setting`.
fn voice_enable_preflight_error() -> Option<String> {
    if crate::voice::voice_mode_enabled::is_voice_mode_enabled() {
        return None;
    }
    Some(
        if crate::utils::auth::is_anthropic_auth_enabled() {
            "Voice mode is not available."
        } else {
            "Voice mode requires a Claude.ai account. Please run /login to sign in."
        }
        .to_string(),
    )
}

/// Copy for the four voice pre-flight checks CC runs after
/// `isVoiceModeEnabled()` (`ConfigTool.ts:251-307`).
///
/// TODO(parity): every trigger sits behind an unported service —
/// `checkRecordingAvailability` / `checkVoiceDependencies` /
/// `requestMicrophonePermission` (`services/voice.ts`) and
/// `isVoiceStreamAvailable` (`services/voiceStreamSTT.ts`). The strings are
/// carried here so the copy stays reviewable against the source and does not
/// drift while the audio stack is unported.
#[allow(dead_code)]
mod voice_preflight_copy {
    /// Maps to: CC `ConfigTool.ts:267` — the fallback when
    /// `checkRecordingAvailability()` reports no reason.
    pub(super) const RECORDING_UNAVAILABLE_ERROR: &str =
        "Voice mode is not available in this environment.";

    /// Maps to: CC `ConfigTool.ts:276` — `isVoiceStreamAvailable()` false.
    pub(super) const STREAM_UNAVAILABLE_ERROR: &str =
        "Voice mode requires a Claude.ai account. Please run /login to sign in.";

    /// Maps to: CC `ConfigTool.ts:286-287` — `checkVoiceDependencies()` false.
    pub(super) fn dependencies_missing_error(install_command: Option<&str>) -> String {
        format!(
            "No audio recording tool found.{}",
            install_command
                .map(|install_command| format!(" Run: {install_command}"))
                .unwrap_or_default()
        )
    }

    /// Maps to: CC `ConfigTool.ts:291-306` — `requestMicrophonePermission()`
    /// false, including the three-platform guidance split.
    pub(super) fn permission_denied_error() -> String {
        let guidance = if cfg!(target_os = "windows") {
            "Settings \u{2192} Privacy \u{2192} Microphone"
        } else if cfg!(target_os = "linux") {
            "your system's audio settings"
        } else {
            "System Settings \u{2192} Privacy & Security \u{2192} Microphone"
        };
        format!("Microphone access is denied. To enable it, go to {guidance}, then try again.")
    }
}

/// Get or set a supported Config setting.
/// Maps to: CC `tools/ConfigTool/ConfigTool.ts:111-411` `call`.
pub(crate) async fn config_output(
    args: &serde_json::Value,
    context: &crate::tool::ToolUseContext,
) -> Output {
    let setting = args
        .get("setting")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("");

    // Voice settings are registered at build time but must also pass the
    // runtime kill-switch; when it is off the setting is unknown so no
    // voice-specific strings leak (CC :113-125).
    let voice_gated_off = cfg!(feature = "voice_mode")
        && setting == "voiceEnabled"
        && !crate::voice::voice_mode_enabled::is_voice_growth_book_enabled();
    let Some(config) = supported_settings::get_config(setting).filter(|_| !voice_gated_off) else {
        return config_error(None, None, format!("Unknown setting: \"{setting}\""));
    };
    let path = supported_settings::get_path(setting);

    let Some(value) = args.get("value").filter(|value| !value.is_null()) else {
        let current_value = read_config_setting_value(config.source, &path);
        let display_value = match config.format_on_read {
            Some(format_on_read) => format_on_read(current_value.as_ref()),
            None => current_value,
        };
        return Output {
            success: true,
            operation: Some("get".to_string()),
            setting: Some(setting.to_string()),
            // CC `:142` assigns `displayValue` straight through, so an unset
            // key stays `undefined` here rather than becoming `null`.
            value: display_value,
            previous_value: None,
            new_value: None,
            error: None,
        };
    };

    // `default` unsets the key so it falls back to the platform-aware default
    // (CC :148-180).
    if setting == "remoteControlAtStartup"
        && value
            .as_str()
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("default"))
    {
        // CC `:156` returns `prev` untouched when the key is already absent,
        // and `saveGlobalConfig` skips the write on reference identity
        // (`config.ts:817-820`). The Rust updater is a `&mut` mutator with no
        // "unchanged" channel, so the same decision is made before the call.
        // CC's `prev` is the locked re-read while this is the cached snapshot,
        // so a concurrent external write of the key in that window would be
        // deleted by CC and skipped here — a strictly smaller effect, and the
        // next `Config` call sees the refreshed cache.
        if crate::utils::config::load_global_config()
            .remote_control_at_startup
            .is_some()
        {
            if let Err(error) = crate::utils::config::save_global_config(|config| {
                config.remote_control_at_startup = None;
            }) {
                return config_error(Some("set"), Some(setting), error.to_string());
            }
        }
        // CC `:161-171` — one `resolved` read AFTER the delete feeds both the
        // AppState projection and the returned value. Same persist-then-project
        // order as the main path below.
        let resolved = crate::utils::config::get_remote_control_at_startup();
        sync_repl_bridge_app_state(context, resolved);
        return Output {
            success: true,
            operation: Some("set".to_string()),
            setting: Some(setting.to_string()),
            value: Some(serde_json::Value::Bool(resolved)),
            previous_value: None,
            new_value: None,
            error: None,
        };
    }

    let mut final_value = value.clone();
    if config.setting_type == supported_settings::SettingType::Boolean {
        if let Some(text) = final_value.as_str() {
            match text.trim().to_ascii_lowercase().as_str() {
                "true" => final_value = serde_json::Value::Bool(true),
                "false" => final_value = serde_json::Value::Bool(false),
                _ => {}
            }
        }
        if !final_value.is_boolean() {
            return config_error(
                Some("set"),
                Some(setting),
                format!("{setting} requires true or false."),
            );
        }
    }

    if let Some(options) = supported_settings::get_options_for_setting(setting) {
        if !options.contains(&setting_value_as_string(&final_value)) {
            return config_error(
                Some("set"),
                Some(setting),
                format!(
                    "Invalid value \"{}\". Options: {}",
                    setting_value_as_string(value),
                    options.join(", ")
                ),
            );
        }
    }

    // Async validation (e.g., model API check) — CC `:216-218`
    // `if (config.validateOnWrite) await config.validateOnWrite(finalValue)`.
    if let Some(validate_on_write) = config.validate_on_write {
        if let Err(error) = validate_on_write(&final_value).await {
            return config_error(Some("set"), Some(setting), error);
        }
    }

    // Pre-flight checks for voice mode (CC :231-308).
    if cfg!(feature = "voice_mode")
        && setting == "voiceEnabled"
        && final_value == serde_json::Value::Bool(true)
    {
        if let Some(error) = voice_enable_preflight_error() {
            return config_error(None, None, error);
        }
    }

    let previous_value = read_config_setting_value(config.source, &path);
    if let Err(error) = write_config_setting_value(config.source, &path, &final_value) {
        return config_error(Some("set"), Some(setting), error);
    }

    // CC `:345-353` (5a) — voice needs `notifyChange` so `applySettingsChange`
    // resyncs `AppState.settings` and the settings cache resets for the next
    // `/voice` read.
    if cfg!(feature = "voice_mode") && setting == "voiceEnabled" {
        crate::utils::settings::change_detector::notify_change(
            crate::utils::settings::SettingSource::User,
        );
    }

    // CC `:355-362` (5b) and `:364-381`. Both projections sit AFTER the write,
    // inside CC's `try`: a throwing persist jumps to the `:400-410` catch and
    // never reaches them. That ordering is not the permission family's
    // fail-closed rule ("never grant what you failed to persist",
    // `permissions.rs:1212`) — nothing here is a privilege. It is the weaker
    // cache rule that happens to point the same way: these AppState fields are
    // a live projection of the persisted config, so projecting a write that
    // did not land would leave the session claiming a value the config does
    // not have, and the next `getGlobalConfig()`/`getInitialSettings()` read
    // would silently contradict it.
    if let Some(app_state_key) = config.app_state_key {
        sync_app_state_key(context, app_state_key, &final_value);
    }

    // The config key differs from the AppState field name, so the generic
    // `appStateKey` mechanism cannot handle this one (CC `:364-366`).
    if setting == "remoteControlAtStartup" {
        sync_repl_bridge_app_state(
            context,
            crate::utils::config::get_remote_control_at_startup(),
        );
    }

    // SEAM (analytics unported): CC emits
    // `logEvent('tengu_config_tool_changed', {setting, value: String(finalValue)})`
    // at `:383-389`, between the AppState projections and the success return.
    // There is no `log_event` in this tree; this is the attach point.

    Output {
        success: true,
        operation: Some("set".to_string()),
        setting: Some(setting.to_string()),
        value: None,
        previous_value,
        new_value: Some(final_value),
        error: None,
    }
}

/// Maps to: CC `ConfigTool.ts:355-362` — the generic `appStateKey` sync, with
/// CC's `prev[appKey] === finalValue → return prev` guard.
///
/// CC assigns `finalValue` straight into the AppState slot because both sides
/// are `unknown`; the three ported slots are typed, so each arm narrows the
/// same way the validation above already guaranteed (`type: 'boolean'`
/// settings were coerced at `:185-201`, `model` passed `getOptions`, which
/// only yields strings). A value that still does not narrow cannot be produced
/// by this call path, and treating it as "no change" keeps the store
/// untouched rather than writing a lie.
fn sync_app_state_key(
    context: &crate::tool::ToolUseContext,
    app_state_key: supported_settings::SyncableAppStateKey,
    final_value: &serde_json::Value,
) {
    use crate::state::store::UpdateDecision;
    use supported_settings::SyncableAppStateKey;

    context.set_app_state_decided(|prev| {
        // Each arm's `(**prev).clone()` is CC's `{...prev}` and the field
        // write is the spread override.
        let next = match app_state_key {
            SyncableAppStateKey::Verbose => {
                let Some(verbose) = final_value.as_bool() else {
                    return UpdateDecision::Same(());
                };
                if prev.verbose == verbose {
                    return UpdateDecision::Same(());
                }
                let mut next = (**prev).clone();
                next.verbose = verbose;
                next
            }
            SyncableAppStateKey::MainLoopModel => {
                let model = setting_value_as_string(final_value);
                if prev.main_loop_model.as_deref() == Some(model.as_str()) {
                    return UpdateDecision::Same(());
                }
                let mut next = (**prev).clone();
                next.main_loop_model = Some(model);
                next
            }
            SyncableAppStateKey::ThinkingEnabled => {
                let Some(thinking_enabled) = final_value.as_bool() else {
                    return UpdateDecision::Same(());
                };
                if prev.thinking_enabled == Some(thinking_enabled) {
                    return UpdateDecision::Same(());
                }
                let mut next = (**prev).clone();
                next.thinking_enabled = Some(thinking_enabled);
                next
            }
        };
        UpdateDecision::Replace {
            next: std::sync::Arc::new(next),
            result: (),
        }
    });
}

/// Maps to: CC `ConfigTool.ts:367-381`, and the block the `"default"` branch
/// runs at `:162-171` — the same updater twice, differing only in line
/// wrapping. One helper carries both.
fn sync_repl_bridge_app_state(context: &crate::tool::ToolUseContext, resolved: bool) {
    use crate::state::store::UpdateDecision;

    context.set_app_state_decided(|prev| {
        // CC `:370-374` / `:164-165`.
        if prev.repl_bridge_enabled == resolved && !prev.repl_bridge_outbound_only {
            return UpdateDecision::Same(());
        }
        let mut next = (**prev).clone();
        next.repl_bridge_enabled = resolved;
        next.repl_bridge_outbound_only = false;
        UpdateDecision::Replace {
            next: std::sync::Arc::new(next),
            result: (),
        }
    });
}

/// Maps to: CC's `String(finalValue)` coercion for option matching (:205), the
/// `${value}` interpolation in the invalid-value copy (:211), and the
/// `appStateKey` assignment for `mainLoopModel` (:360).
///
/// `String(1)` is `"1"`, not `"1.0"` — the coercion goes through the crate's
/// canonical `Number::toString` port rather than `serde_json`'s Display, which
/// keeps the trailing `.0` an f64 literal was parsed with.
pub(super) fn setting_value_as_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Number(number) => number
            .as_f64()
            .map(crate::utils::zod::javascript_number_to_string)
            .unwrap_or_else(|| number.to_string()),
        // `String(true)`/`String(null)` already match `Value::to_string`.
        other => other.to_string(),
    }
}

/// Maps to: CC's `${jsonStringify(v)}` template interpolations at `:418`,
/// `:424` and `:105`.
///
/// `jsonStringify` is bare `JSON.stringify` (`slowOperations.ts:180-194`),
/// which returns the JS value `undefined` — not a string — for an absent
/// field; a template literal then interpolates the literal text `undefined`.
/// This is NOT the same as the JSX interpolation in
/// `ui::json_stringify_interpolation`, where React renders `undefined` as
/// nothing.
fn interpolate_json_stringify(value: Option<&serde_json::Value>) -> String {
    match value {
        Some(value) => serde_json::to_string(value).unwrap_or_else(|_| value.to_string()),
        None => "undefined".to_string(),
    }
}

/// Maps to: CC `tools/ConfigTool/ConfigTool.ts:436-453` `getValue`. The global
/// source reads only `path[0]`; the settings source walks the whole path.
/// `None` is CC's `undefined`.
///
/// Rust-only seam, no CC counterpart: CC tests key PRESENCE (`key in current`)
/// on plain objects parsed from disk, while `load_global_config()` /
/// `get_initial_settings()` return typed snapshots whose `Option::None` fields
/// serialize as JSON `null` — an absent key and an on-disk `null` collapse
/// into the same value before this function can see them. The projection
/// therefore reads a `null` leaf as `undefined`, which is right for the case
/// that actually occurs (key absent from `~/.claude.json` / `settings.json`)
/// and wrong only for a settings file that literally stores `"model": null`,
/// which would read back as `undefined` instead of `'default'`. Undoing that
/// needs raw-JSON reads in `utils/settings`, not a change here.
fn read_config_setting_value(
    source: supported_settings::SettingSource,
    path: &[String],
) -> Option<serde_json::Value> {
    /// The seam above, applied at the leaf.
    fn defined(value: Option<&serde_json::Value>) -> Option<serde_json::Value> {
        match value {
            None | Some(serde_json::Value::Null) => None,
            Some(value) => Some(value.clone()),
        }
    }

    match source {
        // CC `:437-442`.
        supported_settings::SettingSource::Global => {
            let config = serde_json::to_value(crate::utils::config::load_global_config()).ok()?;
            // CC `:439-440` — `const key = path[0]; if (!key) return undefined`,
            // where JS truthiness makes an empty-string key falsy too.
            let key = path.first().filter(|key| !key.is_empty())?;
            defined(config.get(key.as_str()))
        }
        // CC `:443-452`.
        supported_settings::SettingSource::Settings => {
            let settings =
                serde_json::to_value(crate::utils::settings::get_initial_settings()).ok()?;
            let mut current = settings;
            for key in path {
                // CC `:446` — `current && typeof current === 'object' && key in
                // current`; anything else returns undefined. `Value::get`
                // already answers all three for objects, `null`, and scalars.
                current = current.get(key)?.clone();
            }
            defined(Some(&current))
        }
    }
}

/// Maps to: CC `tools/ConfigTool/ConfigTool.ts:312-343` — `saveGlobalConfig` for
/// the global source, `updateSettingsForSource('userSettings', ...)` with
/// `buildNestedObject` (:455-467) for the settings source.
fn write_config_setting_value(
    source: supported_settings::SettingSource,
    path: &[String],
    value: &serde_json::Value,
) -> Result<(), String> {
    match source {
        supported_settings::SettingSource::Global => {
            // CC `:315-325` — the ONLY place the source emits 'Invalid setting
            // path', and it guards `path[0]` inside the global branch only.
            let Some(key) = path.first().filter(|key| !key.is_empty()) else {
                return Err("Invalid setting path".to_string());
            };
            // CC `:326-329` — `saveGlobalConfig(prev => ({...prev, [key]:
            // finalValue}))`. The Rust updater takes a typed struct, so the
            // spread runs on the serialized object and is deserialized back.
            // That round trip is lossless for the same reason CC's object
            // literal is: `GlobalConfig` carries a `#[serde(flatten)]
            // other_global_fields` catch-all (`config.rs:729-732`), so a key
            // the struct does not model is preserved rather than dropped. A
            // value that contradicts a modeled field's type is the one real
            // failure, and it surfaces as serde's own message below.
            let mut object: serde_json::Map<String, serde_json::Value> = serde_json::from_value(
                serde_json::to_value(crate::utils::config::load_global_config())
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            object.insert(key.clone(), value.clone());
            let updated: crate::utils::config::GlobalConfig =
                serde_json::from_value(serde_json::Value::Object(object))
                    .map_err(|error| error.to_string())?;
            crate::utils::config::save_global_config(|config| *config = updated)
                .map_err(|error| error.to_string())
        }
        supported_settings::SettingSource::Settings => {
            // CC `buildNestedObject` (:455-467): an empty path yields `{}`,
            // which `updateSettingsForSource` applies as a no-op — no error.
            let mut root = serde_json::Map::new();
            if let Some((key, rest)) = path.split_first() {
                let mut update = value.clone();
                for segment in rest.iter().rev() {
                    update = serde_json::json!({ segment.clone(): update });
                }
                root.insert(key.clone(), update);
            }
            crate::utils::settings::update_settings_for_source(
                crate::utils::settings::SettingSource::User,
                &root,
            )
            .map_err(|error| error.to_string())
        }
    }
}

/// Behavioral half of CC `ConfigTool` — dispatched via `crate::tool::ToolCall`.
pub(crate) struct ConfigTool;

impl crate::tool::ToolCall for ConfigTool {
    fn name(&self) -> &'static str {
        "Config"
    }

    /// Maps to: CC `ConfigTool.ts:74-76` `async prompt() { return
    /// generatePrompt() }` — same source the wire schema renders eagerly.
    fn prompt(
        &self,
        _tool: &crate::types::tools::Tool,
        _options: &crate::tool::ToolPromptOptions<'_>,
    ) -> String {
        prompt::generate_prompt()
    }

    /// Maps to: CC `ConfigTool.searchHint` (:69) — ToolSearch ranks a
    /// word-bound hit here above one in the prompt
    /// (`ToolSearchTool.ts:282`).
    fn search_hint(&self) -> Option<&'static str> {
        Some("get or set Claude Code settings (theme, model)")
    }

    /// Maps to: CC `ConfigTool.description()` (:71-73) — the `DESCRIPTION`
    /// constant, not the generated prompt. Read by `useCanUseTool.tsx:138-143`
    /// for the permission dialog / swarm relay / auto-mode denial display.
    fn description(&self, _args: &serde_json::Value) -> String {
        prompt::DESCRIPTION.to_string()
    }

    /// Maps to: CC `ConfigTool.isConcurrencySafe()` (ConfigTool.ts:87) — true.
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }

    /// Maps to: CC `ConfigTool.shouldDefer` (:86).
    fn should_defer(&self) -> bool {
        true
    }

    /// Maps to: CC `ConfigTool.userFacingName()` (:83-85).
    fn user_facing_name(&self, _args: Option<&serde_json::Value>) -> String {
        constants::CONFIG_TOOL_NAME.to_string()
    }

    /// Maps to: CC `ConfigTool.isReadOnly(input)` (:90-92).
    fn is_read_only(&self, args: &serde_json::Value) -> bool {
        !is_set_operation(args)
    }

    /// Maps to: CC `ConfigTool.toAutoClassifierInput(input)` (:93-97).
    fn to_auto_classifier_input(&self, args: &serde_json::Value) -> String {
        let setting = args
            .get("setting")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        match args.get("value").filter(|value| !value.is_null()) {
            Some(value) => format!("{setting} = {}", setting_value_as_string(value)),
            None => setting.to_string(),
        }
    }

    /// Maps to: CC `ConfigTool.checkPermissions(input)` (:98-107) — reads are
    /// auto-allowed, writes ask.
    fn check_permissions(
        &self,
        args: &serde_json::Value,
        _context: &crate::tool::ToolUseContext,
    ) -> crate::utils::permissions::permission_result::PermissionResult {
        use crate::utils::permissions::permission_result::PermissionResult;

        if !is_set_operation(args) {
            return PermissionResult::Allow {
                updated_input: Some(args.clone()),
                user_modified: None,
                decision_reason: None,
                tool_use_id: None,
                accept_feedback: None,
                content_blocks: Vec::new(),
            };
        }
        let setting = args
            .get("setting")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        // CC `:105` — the same `jsonStringify` interpolation as the result
        // copy. The guard above already returned for `value === undefined`, so
        // the `undefined` arm is unreachable from here.
        let value = interpolate_json_stringify(args.get("value"));
        PermissionResult::Ask {
            message: format!("Set {setting} to {value}"),
            updated_input: None,
            decision_reason: None,
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_bash_security_check_for_misparsing: false,
            pending_classifier_check: None,
            content_blocks: Vec::new(),
        }
    }

    fn call<'a>(
        &'a self,
        args: &'a serde_json::Value,
        request: &'a crate::types::permissions::PermissionRequest,
        context: &'a crate::tool::ToolUseContext,
        _can_use_tool: Option<crate::tool::CanUseToolFn<'a>>,
        _parent_message: Option<&'a crate::types::message::AssistantMessage>,
        _on_progress: Option<crate::tool::ToolCallProgressFn<'a>>,
    ) -> futures::future::BoxFuture<'a, crate::tool::ToolResult> {
        Box::pin(async move {
            let _ = request;
            crate::tool::ToolResult {
                // CC `call({setting, value}, context)` (:111) — the second
                // parameter is what carries `setAppState` into the write path.
                data: crate::tool::ToolOutput::Config(config_output(args, context).await),
                new_messages: Vec::new(),
            }
        })
    }

    /// Maps to: CC `tools/ConfigTool/ConfigTool.ts`
    /// `mapToolResultToToolResultBlockParam` (:418-433).
    fn map_tool_result_to_tool_result_block_param(
        &self,
        data: &crate::tool::ToolOutput,
        _tool_use_id: &str,
    ) -> (String, crate::types::message::ToolResultStatus) {
        match data {
            crate::tool::ToolOutput::Config(output) => {
                if output.success {
                    if output.operation.as_deref() == Some("get") {
                        return (
                            format!(
                                "{} = {}",
                                output.setting.as_deref().unwrap_or_default(),
                                interpolate_json_stringify(output.value.as_ref())
                            ),
                            crate::types::message::ToolResultStatus::Success,
                        );
                    }
                    return (
                        format!(
                            "Set {} to {}",
                            output.setting.as_deref().unwrap_or_default(),
                            interpolate_json_stringify(output.new_value.as_ref())
                        ),
                        crate::types::message::ToolResultStatus::Success,
                    );
                }
                (
                    format!("Error: {}", output.error.as_deref().unwrap_or_default()),
                    crate::types::message::ToolResultStatus::Error,
                )
            }
            crate::tool::ToolOutput::Composed {
                content, status, ..
            } => (content.clone(), *status),
            _ => (
                "<tool_use_error>Config returned an unexpected output variant</tool_use_error>"
                    .to_string(),
                crate::types::message::ToolResultStatus::Error,
            ),
        }
    }

    /// Maps to: CC recording ConfigTool's `Output` as the message's
    /// `toolUseResult`.
    fn tool_use_result(&self, data: &crate::tool::ToolOutput) -> Option<serde_json::Value> {
        match data {
            crate::tool::ToolOutput::Config(output) => Some(ui::output_to_value(output)),
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
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn config_tool_schema_matches_official_input_shape() {
        let schema = super::config_tool_schema();
        assert_eq!(schema.name, "Config");
        assert_eq!(
            schema.input_schema.get("required"),
            Some(&serde_json::json!(["setting"]))
        );
        // `value` is a `z.union`, which zod projects as `anyOf` — not the
        // `"type": [...]` shorthand this assertion used to pin.
        assert_eq!(
            schema.input_schema.pointer("/properties/value/anyOf"),
            Some(&serde_json::json!([
                {"type": "string"},
                {"type": "boolean"},
                {"type": "number"},
            ]))
        );
        assert!(
            schema
                .description
                .contains("Get or set Claude Code configuration settings")
        );
    }

    #[tokio::test]
    async fn config_tool_call_returns_official_output_schema_and_model_copy() {
        use crate::tool::ToolCall;

        let args = serde_json::json!({"setting": "theme"});
        let request = crate::utils::permissions::permissions::mock_permission_request_with_input(
            "perm-config".to_string(),
            "toolu_config".to_string(),
            "Config".to_string(),
            "theme".to_string(),
            args.clone(),
            crate::types::permissions::PermissionMode::Default,
        );
        let tool = super::ConfigTool;
        let result = tool
            .call(
                &args,
                &request,
                &crate::tool::ToolUseContext::default(),
                None,
                None,
                None,
            )
            .await;

        let crate::tool::ToolOutput::Config(output) = result.data else {
            panic!("Config should return its official ToolOutput variant");
        };
        assert!(output.success);
        assert_eq!(output.operation.as_deref(), Some("get"));
        assert_eq!(output.setting.as_deref(), Some("theme"));
        assert!(output.value.is_some());

        let data = crate::tool::ToolOutput::Config(output);
        let (content, status) =
            tool.map_tool_result_to_tool_result_block_param(&data, "toolu_config");
        assert_eq!(status, crate::types::message::ToolResultStatus::Success);
        assert!(content.starts_with("theme = "));
        // No display shape — the trait projects the raw Output object.
        let raw = tool
            .tool_use_result(&data)
            .expect("raw output should ride the row");
        assert_eq!(raw.get("success"), Some(&serde_json::json!(true)));
        assert_eq!(raw.get("operation"), Some(&serde_json::json!("get")));
        assert_eq!(raw.get("setting"), Some(&serde_json::json!("theme")));
        assert!(raw.get("value").is_some());
    }

    /// Maps to: CC `ConfigTool.ts:196-212` — write validation rejects before any
    /// mutation, so these paths never touch disk.
    #[tokio::test]
    async fn config_set_rejects_invalid_values_with_official_strings() {
        let context = crate::tool::ToolUseContext::default();
        let boolean = super::config_output(
            &serde_json::json!({
                "setting": "verbose",
                "value": "maybe",
            }),
            &context,
        )
        .await;
        assert!(!boolean.success);
        assert_eq!(boolean.operation.as_deref(), Some("set"));
        assert_eq!(
            boolean.error.as_deref(),
            Some("verbose requires true or false.")
        );

        let options = super::config_output(
            &serde_json::json!({
                "setting": "theme",
                "value": "solarized",
            }),
            &context,
        )
        .await;
        assert!(!options.success);
        assert!(
            options
                .error
                .as_deref()
                .is_some_and(|error| error.starts_with("Invalid value \"solarized\". Options: "))
        );

        let unknown = super::config_output(
            &serde_json::json!({
                "setting": "notASetting",
                "value": true,
            }),
            &context,
        )
        .await;
        assert!(!unknown.success);
        assert_eq!(
            unknown.error.as_deref(),
            Some("Unknown setting: \"notASetting\"")
        );
    }

    /// Maps to: CC `ConfigTool.ts:216-218` — `validateOnWrite` runs after
    /// options and before persist. A model rejected by the picker or the
    /// owner slot must not land in settings.
    #[tokio::test]
    async fn config_set_does_not_persist_a_model_rejected_on_write() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let fixture = super::prompt::tests::ModelSectionFixture::new();
        fixture.write_settings(r#"{"availableModels":["haiku"]}"#);

        let context = crate::tool::ToolUseContext::default();
        let output = super::config_output(
            &serde_json::json!({"setting": "model", "value": "opus"}),
            &context,
        )
        .await;
        assert!(!output.success);
        assert_eq!(output.operation.as_deref(), Some("set"));
        assert!(output.error.is_some());

        let read = super::config_output(&serde_json::json!({"setting": "model"}), &context).await;
        assert!(read.success);
        assert_eq!(
            read.value, None,
            "a rejected model write must leave the key unset"
        );
    }

    /// Maps to: CC `ConfigTool.ts:436-453` + `:142` + `:418` — `getValue`
    /// returns JS `undefined` for an unset key, the get branch passes it
    /// straight into `data.value`, and the tool_result template interpolates
    /// `JSON.stringify(undefined)` as the literal text `undefined`.
    ///
    /// `model` is the settings-source case and the one whose `formatOnRead`
    /// (`supportedSettings.ts:105`) proves the distinction is a VALUE
    /// difference, not formatting: `undefined !== null`, so the unset read is
    /// `undefined`, never `'default'`.
    #[tokio::test]
    async fn config_get_reads_an_unset_setting_back_as_undefined_not_null() {
        use crate::tool::ToolCall;

        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let fixture = super::prompt::tests::ModelSectionFixture::new();
        fixture.write_settings("{}");

        let context = crate::tool::ToolUseContext::default();
        let output = super::config_output(&serde_json::json!({"setting": "model"}), &context).await;
        assert!(output.success);
        assert_eq!(output.operation.as_deref(), Some("get"));
        assert_eq!(
            output.value, None,
            "an unset key is JS undefined, and formatOnRead's `v === null` does not catch it"
        );

        let tool = super::ConfigTool;
        let data = crate::tool::ToolOutput::Config(output);
        let (content, status) = tool.map_tool_result_to_tool_result_block_param(&data, "toolu_cfg");
        assert_eq!(status, crate::types::message::ToolResultStatus::Success);
        assert_eq!(content, "model = undefined");

        // `JSON.stringify` DROPS an undefined property, so the wire shape has
        // no `value` key at all (CC `outputSchema` `.optional()`).
        let raw = tool
            .tool_use_result(&data)
            .expect("raw output rides the row");
        assert_eq!(
            raw,
            serde_json::json!({"success": true, "operation": "get", "setting": "model"})
        );
        // And the transcript renderer interpolates it as JSX, where React
        // renders `undefined` as nothing (UI.tsx:29-33).
        let lines = super::ui::render_tool_result_message(Some(&raw));
        assert_eq!(lines[0].text, "model = ");
    }

    /// Maps to: CC `ConfigTool.ts:436-453` — a value that IS present reads back
    /// as itself, and a settings file that literally stores JSON `null` is the
    /// only way `formatOnRead`'s `'default'` branch fires.
    #[tokio::test]
    async fn config_get_reads_a_present_setting_back_as_its_value() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let fixture = super::prompt::tests::ModelSectionFixture::new();
        fixture.write_settings(r#"{"model":"opus"}"#);

        let context = crate::tool::ToolUseContext::default();
        let output = super::config_output(&serde_json::json!({"setting": "model"}), &context).await;
        assert_eq!(output.value, Some(serde_json::json!("opus")));

        // The global source is the other half of `getValue`. `theme` carries a
        // factory default (CC `config.ts:590` `createDefaultGlobalConfig`,
        // ported at `config.rs:899`), so it is never undefined — but only
        // through the real loader, since the fixture's in-memory override is a
        // bare `GlobalConfig::default()`.
        use_real_global_config_file();
        let theme = super::config_output(&serde_json::json!({"setting": "theme"}), &context).await;
        assert_eq!(theme.value, Some(serde_json::json!("dark")));

        // An unset global key with NO factory default stays undefined, the
        // same as the settings source.
        let teammate_mode =
            super::config_output(&serde_json::json!({"setting": "teammateMode"}), &context).await;
        assert_eq!(teammate_mode.value, None);
    }

    /// Maps to: CC `ConfigTool.ts:355-362` — the generic `appStateKey` sync,
    /// which runs AFTER the persist and inside the `try`. All three syncable
    /// keys (`supportedSettings.ts:46/:94/:111`) land in one pass.
    #[tokio::test]
    async fn config_set_projects_the_official_app_state_keys_after_the_write() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let fixture = super::prompt::tests::ModelSectionFixture::new();
        fixture.write_settings("{}");

        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let mut context = crate::tool::ToolUseContext::default();
        context.app_store = crate::tool::AppStoreRef::new(store.clone());
        assert!(!store.get().verbose);
        assert_eq!(store.get().main_loop_model, None);
        assert_eq!(store.get().thinking_enabled, None);

        // `verbose` — global source, boolean coerced from the string form.
        let verbose = super::config_output(
            &serde_json::json!({"setting": "verbose", "value": "true"}),
            &context,
        )
        .await;
        assert!(verbose.success, "{:?}", verbose.error);
        assert!(store.get().verbose);

        // `model` — settings source, string.
        let model = super::config_output(
            &serde_json::json!({"setting": "model", "value": "haiku"}),
            &context,
        )
        .await;
        assert!(model.success, "{:?}", model.error);
        assert_eq!(store.get().main_loop_model.as_deref(), Some("haiku"));

        // `alwaysThinkingEnabled` — settings source, boolean.
        let thinking = super::config_output(
            &serde_json::json!({"setting": "alwaysThinkingEnabled", "value": false}),
            &context,
        )
        .await;
        assert!(thinking.success, "{:?}", thinking.error);
        assert_eq!(store.get().thinking_enabled, Some(false));

        // A setting with no `appStateKey` leaves the store alone.
        let before = store.get();
        let language = super::config_output(
            &serde_json::json!({"setting": "language", "value": "japanese"}),
            &context,
        )
        .await;
        assert!(language.success, "{:?}", language.error);
        assert_eq!(store.get(), before);
    }

    /// Maps to: CC `ConfigTool.ts:358-360` — `if (prev[appKey] === finalValue)
    /// return prev`. A no-op write must not install a new root.
    #[tokio::test]
    async fn config_set_keeps_the_official_unchanged_guard() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let fixture = super::prompt::tests::ModelSectionFixture::new();
        fixture.write_settings("{}");

        let mut initial = crate::state::app_state_store::AppState::default();
        initial.verbose = true;
        let store = crate::state::store::AppStore::new(initial, None);
        let mut context = crate::tool::ToolUseContext::default();
        context.app_store = crate::tool::AppStoreRef::new(store.clone());
        let before = store.get();

        let output = super::config_output(
            &serde_json::json!({"setting": "verbose", "value": true}),
            &context,
        )
        .await;
        assert!(output.success, "{:?}", output.error);
        assert!(
            std::sync::Arc::ptr_eq(&before, &store.get()),
            "an unchanged value must return prev, not a fresh root"
        );
    }

    /// Maps to: CC `ConfigTool.ts:400-410` — a failing persist jumps to the
    /// catch, so neither projection runs. The Rust write path returns `Err`
    /// where CC throws; both must leave AppState untouched.
    #[tokio::test]
    async fn config_set_does_not_project_a_write_that_failed() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let fixture = super::prompt::tests::ModelSectionFixture::new();
        fixture.write_settings("{}");

        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let mut context = crate::tool::ToolUseContext::default();
        context.app_store = crate::tool::AppStoreRef::new(store.clone());
        let before = store.get();

        // An unparseable settings file is the reachable persist failure:
        // `updateSettingsForSource` re-reads the file before merging, so the
        // write returns the parse error instead of landing.
        fixture.write_settings("{ this is not json");

        let output = super::config_output(
            &serde_json::json!({"setting": "model", "value": "haiku"}),
            &context,
        )
        .await;
        assert!(!output.success);
        assert_eq!(output.operation.as_deref(), Some("set"));
        assert!(output.error.is_some(), "the persist error is reported");
        assert_eq!(
            store.get().main_loop_model,
            None,
            "the appStateKey projection sits after the write, inside CC's try"
        );
        assert!(std::sync::Arc::ptr_eq(&before, &store.get()));
    }

    /// Maps to: CC `ConfigTool.ts:315-325` — 'Invalid setting path' is emitted
    /// for exactly one condition, the falsy `path[0]` in the global branch.
    /// The settings branch has no such guard: `buildNestedObject([])` is `{}`,
    /// which `updateSettingsForSource` applies as a no-op.
    ///
    /// The port used to reuse this copy for two invented conditions in the
    /// global branch. Both are gone: `#[serde(flatten)] other_global_fields`
    /// (`config.rs:729-732`) makes the typed round trip lossless for unmodeled
    /// keys, exactly like CC's object literal, so there was nothing left to
    /// guard.
    #[test]
    fn config_write_reserves_the_official_invalid_path_copy_for_the_official_case() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _fixture = super::prompt::tests::ModelSectionFixture::new();
        use_real_global_config_file();

        assert_eq!(
            super::write_config_setting_value(
                super::supported_settings::SettingSource::Global,
                &[],
                &serde_json::json!(true),
            ),
            Err("Invalid setting path".to_string())
        );
        assert_eq!(
            super::write_config_setting_value(
                super::supported_settings::SettingSource::Global,
                &[String::new()],
                &serde_json::json!(true),
            ),
            Err("Invalid setting path".to_string()),
            "JS truthiness makes an empty-string key falsy too"
        );
        // An unmodeled key is NOT an error — CC spreads it onto the object and
        // so does the port, via the flatten catch-all.
        assert_eq!(
            super::write_config_setting_value(
                super::supported_settings::SettingSource::Global,
                &["notAModeledKey".to_string()],
                &serde_json::json!(true),
            ),
            Ok(())
        );
    }

    /// Maps to: CC `ConfigTool.ts:205`/`:211` — `String(finalValue)`.
    /// `String(1)` is `"1"`; `serde_json`'s Display keeps the `1.0` an f64
    /// literal parsed with, which would miss a numeric option and print the
    /// wrong value in the rejection copy.
    #[test]
    fn config_coerces_values_with_javascript_number_semantics() {
        assert_eq!(super::setting_value_as_string(&serde_json::json!(1.0)), "1");
        assert_eq!(
            super::setting_value_as_string(&serde_json::json!(1.5)),
            "1.5"
        );
        assert_eq!(
            super::setting_value_as_string(&serde_json::json!(-0.0)),
            "0",
            "JS has no negative-zero string"
        );
        assert_eq!(
            super::setting_value_as_string(&serde_json::json!(1e21)),
            "1e+21"
        );
        assert_eq!(
            super::setting_value_as_string(&serde_json::json!(true)),
            "true"
        );
        assert_eq!(
            super::setting_value_as_string(&serde_json::json!("dark")),
            "dark",
            "a string coerces to itself, without JSON quotes"
        );
    }

    /// Points `load_global_config()` at the fixture's temp dir instead of the
    /// in-memory test override, so `saveGlobalConfig` round-trips through a
    /// real file the way CC's does.
    fn use_real_global_config_file() {
        crate::utils::config::set_test_global_config(None);
        crate::utils::config::clear_global_config_cache_for_testing();
    }

    /// Maps to: CC `ConfigTool.ts:155-160` — the `saveGlobalConfig` updater
    /// returns `prev` untouched when the key is already absent, and
    /// `saveGlobalConfig` skips the write on reference identity
    /// (`config.ts:817-820`). A `Config remoteControlAtStartup=default` on a
    /// config that never set the key must not create or touch the file.
    #[tokio::test]
    async fn config_remote_control_default_skips_the_write_when_the_key_is_absent() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _fixture = super::prompt::tests::ModelSectionFixture::new();
        use_real_global_config_file();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");

        let path = crate::utils::env::get_global_claude_file();
        assert!(
            !path.exists(),
            "fixture starts without a global config file"
        );

        let context = crate::tool::ToolUseContext::default();
        let output = super::config_output(
            &serde_json::json!({"setting": "remoteControlAtStartup", "value": "default"}),
            &context,
        )
        .await;
        assert!(output.success, "{:?}", output.error);
        assert_eq!(output.operation.as_deref(), Some("set"));
        // CC `:177` returns the RESOLVED value, not previousValue/newValue.
        assert_eq!(output.value, Some(serde_json::json!(false)));
        assert_eq!(output.previous_value, None);
        assert_eq!(output.new_value, None);
        assert!(
            !path.exists(),
            "an already-absent key means the updater returns prev, so no write happens"
        );

        // With the key present the delete DOES land.
        std::fs::write(&path, r#"{"remoteControlAtStartup":true}"#).unwrap();
        crate::utils::config::clear_global_config_cache_for_testing();
        let output = super::config_output(
            &serde_json::json!({"setting": "remoteControlAtStartup", "value": "DEFAULT "}),
            &context,
        )
        .await;
        assert!(output.success, "{:?}", output.error);
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written.get("remoteControlAtStartup"), None);
    }

    /// Maps to: CC `ConfigTool.ts:162-171` and `:367-381` — the two copies of
    /// the bridge projection. `replBridgeEnabled` takes the RESOLVED value and
    /// `replBridgeOutboundOnly` is forced false, because the config key's name
    /// differs from the AppState field's and the generic `appStateKey`
    /// mechanism cannot express it.
    #[tokio::test]
    async fn config_remote_control_projects_both_bridge_fields() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _fixture = super::prompt::tests::ModelSectionFixture::new();
        use_real_global_config_file();
        let _writes = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");

        let mut initial = crate::state::app_state_store::AppState::default();
        initial.repl_bridge_outbound_only = true;
        let store = crate::state::store::AppStore::new(initial, None);
        let mut context = crate::tool::ToolUseContext::default();
        context.app_store = crate::tool::AppStoreRef::new(store.clone());

        // The ordinary set branch (CC `:367-381`).
        let output = super::config_output(
            &serde_json::json!({"setting": "remoteControlAtStartup", "value": true}),
            &context,
        )
        .await;
        assert!(output.success, "{:?}", output.error);
        assert!(store.get().repl_bridge_enabled);
        assert!(!store.get().repl_bridge_outbound_only);

        // The "default" branch (CC `:162-171`), which unsets the key and
        // projects whatever `getRemoteControlAtStartup()` then resolves to.
        let output = super::config_output(
            &serde_json::json!({"setting": "remoteControlAtStartup", "value": "default"}),
            &context,
        )
        .await;
        assert!(output.success, "{:?}", output.error);
        assert!(!store.get().repl_bridge_enabled);
        assert!(!store.get().repl_bridge_outbound_only);

        // CC `:370-374` — already-matching state returns prev.
        let before = store.get();
        let output = super::config_output(
            &serde_json::json!({"setting": "remoteControlAtStartup", "value": "default"}),
            &context,
        )
        .await;
        assert!(output.success, "{:?}", output.error);
        assert!(std::sync::Arc::ptr_eq(&before, &store.get()));
    }

    /// Maps to: CC `ConfigTool.ts:69` `searchHint` and `:71-73` `description()`
    /// → `prompt.ts:9` `DESCRIPTION`. `description()` is the permission-dialog
    /// line (`useCanUseTool.tsx:138-143`), NOT the generated model prompt.
    #[test]
    fn config_exposes_the_official_description_and_search_hint() {
        use crate::tool::ToolCall;

        // `generate_prompt()` below reads the model picker, so pin the process
        // state instead of the developer's own config.
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _fixture = super::prompt::tests::ModelSectionFixture::new();

        let tool = super::ConfigTool;
        assert_eq!(
            tool.description(&serde_json::json!({"setting": "theme"})),
            "Get or set Claude Code configuration settings."
        );
        assert_eq!(
            tool.search_hint(),
            Some("get or set Claude Code settings (theme, model)")
        );
        // The prompt is a different, much longer string that merely starts
        // with the same sentence.
        assert_ne!(
            tool.description(&serde_json::json!({})),
            super::prompt::generate_prompt()
        );
    }

    /// Maps to: CC `ConfigTool.ts:239-306` — the voice pre-flight copy. Only
    /// the first check has ported inputs; the rest are pinned as strings.
    #[test]
    fn voice_preflight_copy_matches_the_official_strings() {
        use super::voice_preflight_copy;

        // CC :246 / :276 share one string; :248 is the authenticated variant.
        // Which one the pre-flight picks depends on ambient credentials, so pin
        // the copy rather than the developer's login state.
        const REQUIRES_ACCOUNT: &str =
            "Voice mode requires a Claude.ai account. Please run /login to sign in.";
        const NOT_AVAILABLE: &str = "Voice mode is not available.";
        match super::voice_enable_preflight_error() {
            Some(error) => assert!(
                error == REQUIRES_ACCOUNT || error == NOT_AVAILABLE,
                "unexpected pre-flight copy: {error}"
            ),
            None => assert!(crate::voice::voice_mode_enabled::is_voice_mode_enabled()),
        }
        assert_eq!(
            voice_preflight_copy::STREAM_UNAVAILABLE_ERROR,
            REQUIRES_ACCOUNT
        );
        assert_eq!(
            voice_preflight_copy::RECORDING_UNAVAILABLE_ERROR,
            "Voice mode is not available in this environment."
        );
        assert_eq!(
            voice_preflight_copy::dependencies_missing_error(None),
            "No audio recording tool found."
        );
        assert_eq!(
            voice_preflight_copy::dependencies_missing_error(Some("brew install sox")),
            "No audio recording tool found. Run: brew install sox"
        );
        let guidance = if cfg!(target_os = "windows") {
            "Settings \u{2192} Privacy \u{2192} Microphone"
        } else if cfg!(target_os = "linux") {
            "your system's audio settings"
        } else {
            "System Settings \u{2192} Privacy & Security \u{2192} Microphone"
        };
        assert_eq!(
            voice_preflight_copy::permission_denied_error(),
            format!("Microphone access is denied. To enable it, go to {guidance}, then try again.")
        );
    }

    /// Maps to: CC `ConfigTool.ts:90-107`.
    #[test]
    fn config_read_only_and_check_permissions_follow_value_presence() {
        use crate::tool::ToolCall;
        use crate::utils::permissions::permission_result::PermissionResult;

        let tool = super::ConfigTool;
        let context = crate::tool::ToolUseContext::default();
        let get = serde_json::json!({"setting": "theme"});
        let set = serde_json::json!({"setting": "theme", "value": "dark"});

        assert!(tool.is_read_only(&get));
        assert!(!tool.is_read_only(&set));
        assert_eq!(tool.to_auto_classifier_input(&get), "theme");
        assert_eq!(tool.to_auto_classifier_input(&set), "theme = dark");

        assert!(matches!(
            tool.check_permissions(&get, &context),
            PermissionResult::Allow { .. }
        ));
        assert!(matches!(
            tool.check_permissions(&set, &context),
            PermissionResult::Ask { message, .. } if message == "Set theme to \"dark\""
        ));
    }
}
