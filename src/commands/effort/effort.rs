//! Maps to: CC `commands/effort/effort.tsx` (2.1.198+ picker dispatch).
//!
//! Empty `/effort` opens the slider; args share its cache-break confirmation. `xhigh` / `max` are first-class apply options. `ultracode`
//! follows official [`crate::utils::ultracode::is_ultracode_available`]
//! (`tengu_workflows_enabled`). Workflow orchestration is not ported.
//!
//! CC's `ShowCurrentEffort` and `ApplyEffortAndClose` components synchronously
//! call `onDone` (the latter from an effect). The established typed-command L1
//! maps that null-rendering JSX path to [`call`], which returns the same message
//! and applies `EffortCommandResult.effortUpdate` through `ToolUseContext`'s
//! live AppStore. Settings persistence stays in `utils/settings`, and effort
//! resolution stays in `utils/effort`; this file owns command behavior only.
//!
//! Deliberate omission: `tengu_effort_command` analytics is telemetry and is
//! outside this project's scope. It does not affect command output or state.

use crate::constants::xml::COMMON_HELP_ARGS;
use crate::state::store::AppStore;
use crate::tool::ToolUseContext;
use crate::utils::effort::{
    EffortValue, get_displayed_effort_level, get_effort_env_override, get_effort_value_description,
    model_supports_xhigh_effort, to_persistable_effort,
};
use crate::utils::settings::{SettingSource, update_settings_for_source};
use crate::utils::ultracode::{
    MAX_CAPABLE_MODELS, XHIGH_CAPABLE_MODELS, clamp_effort_to_org_limit,
    get_eligible_effort_levels, is_effort_within_org_limit, is_launch_effort_pinned,
    is_ultracode_active, is_ultracode_available, parse_extended_effort_level,
    should_confirm_effort_cache_break, unpin_launch_effort,
};

/// Maps to: CC `_Zi` — valid-options list for `/effort` errors.
fn get_valid_options_list(model: &str) -> String {
    let mut parts = get_eligible_effort_levels(model);
    if is_ultracode_available(Some(model)) {
        parts.push("ultracode");
    }
    parts.push("auto");
    parts.join(", ")
}

/// Maps to: CC `UhE`.
fn usage_description(level: &str) -> String {
    match level {
        "low" => "Quick, straightforward implementation".to_string(),
        "medium" => "Balanced approach with standard testing".to_string(),
        "high" => "Comprehensive implementation with extensive testing".to_string(),
        "xhigh" => format!("Extended reasoning with thorough analysis ({XHIGH_CAPABLE_MODELS})"),
        "max" => format!("Maximum capability with deepest reasoning ({MAX_CAPABLE_MODELS})"),
        _ => crate::utils::effort::get_effort_level_description(level).to_string(),
    }
}

/// Maps to: CC `commands/effort/effort.tsx:21-24::EffortCommandResult`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffortCommandResult {
    pub message: String,
    /// `None` = no update; `Some(None)` = clear effort; `Some(Some(value))` = set.
    pub effort_update: Option<Option<EffortValue>>,
    /// Set together with `effort_update`. Explicit levels clear the flag.
    pub ultracode: bool,
}

/// Maps to: CC `commands/effort/effort.tsx#call` empty-args picker branch (`Zsm`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EffortCall {
    OpenPicker,
    ConfirmArgs(String),
    Result(EffortCommandResult),
}

impl EffortCall {
    pub fn into_result(self) -> Option<EffortCommandResult> {
        match self {
            Self::Result(result) => Some(result),
            Self::OpenPicker | Self::ConfirmArgs(_) => None,
        }
    }
}

impl EffortCommandResult {
    fn message(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            effort_update: None,
            ultracode: false,
        }
    }

    fn with_update(message: impl Into<String>, value: Option<EffortValue>) -> Self {
        Self {
            message: message.into(),
            effort_update: Some(value),
            ultracode: false,
        }
    }

    fn with_ultracode(message: impl Into<String>, value: EffortValue) -> Self {
        Self {
            message: message.into(),
            effort_update: Some(Some(value)),
            ultracode: true,
        }
    }
}

/// Maps to: CC `bZi` (`getUsageText`).
pub fn usage_text(model: &str) -> String {
    let ultracode = is_ultracode_available(Some(model));
    let levels = get_eligible_effort_levels(model);
    let mut text = format!(
        "Usage: /effort [{}{}|auto]\n\nEffort levels:\n",
        levels.join("|"),
        if ultracode { "|ultracode" } else { "" }
    );
    for level in levels {
        text.push_str(&format!("- {level}: {}\n", usage_description(level)));
    }
    if ultracode {
        text.push_str("- ultracode: xhigh + dynamic workflow orchestration (this session only)\n");
    }
    text.push_str("- auto: Use the default effort level for your model");
    text
}

/// Maps to: CC EffortPicker Esc `onDone('Cancelled')`.
pub fn handle_cancel_output() -> String {
    "Cancelled".to_string()
}

fn update_effort_setting(value: Option<&str>) -> anyhow::Result<()> {
    let update = value
        .map(|value| serde_json::Value::String(value.to_string()))
        // Rust `null` is the settings adapter's carrier for JS `undefined`.
        .unwrap_or(serde_json::Value::Null);
    update_settings_for_source(
        SettingSource::User,
        &serde_json::Map::from_iter([("effortLevel".to_string(), update)]),
    )
}

/// Maps to: CC `commands/effort/effort.tsx:26-67::setEffortValue` / 2.1.241 `BhE`.
fn set_effort_value(effort_value: EffortValue, model: &str) -> EffortCommandResult {
    let clamped = match &effort_value {
        EffortValue::Named(level) => EffortValue::Named(clamp_effort_to_org_limit(level, model)),
        _ => effort_value.clone(),
    };
    let was_clamped = effort_value != clamped;
    let persistable = to_persistable_effort(Some(&clamped));
    if let Some(value) = persistable.as_deref() {
        if let Err(error) = update_effort_setting(Some(value)) {
            return EffortCommandResult::message(format!("Failed to set effort level: {error}"));
        }
    }

    unpin_launch_effort();
    let env_conflicts = match get_effort_env_override() {
        Some(Some(env_value)) => env_value != clamped,
        Some(None) => true,
        None => false,
    };
    if env_conflicts {
        let env_raw = crate::utils::process_env::var("CLAUDE_CODE_EFFORT_LEVEL").unwrap_or_default();
        if persistable.is_none() {
            return EffortCommandResult::with_update(
                format!(
                    "Not applied: CLAUDE_CODE_EFFORT_LEVEL={env_raw} overrides effort this session, and {} is session-only (nothing saved)",
                    clamped.as_str()
                ),
                Some(clamped),
            );
        }
        return EffortCommandResult::with_update(
            format!(
                "CLAUDE_CODE_EFFORT_LEVEL={env_raw} overrides this session — clear it and {} takes over",
                clamped.as_str()
            ),
            Some(clamped),
        );
    }

    let description = get_effort_value_description(&clamped);
    let suffix = if persistable.is_some() {
        " (saved as your default for new sessions)"
    } else {
        " (this session only)"
    };
    if was_clamped {
        return EffortCommandResult::with_update(
            format!(
                "Effort '{}' exceeds your organization's limit for {model}; set to '{}' instead{suffix}: {description}",
                effort_value.as_str(),
                clamped.as_str()
            ),
            Some(clamped),
        );
    }
    EffortCommandResult::with_update(
        format!(
            "Set effort level to {}{suffix}: {description}",
            clamped.as_str()
        ),
        Some(clamped),
    )
}

/// Maps to: CC `commands/effort/effort.tsx#showCurrentEffort` (`pdr`).
pub fn show_current_effort(
    app_state_effort: Option<&EffortValue>,
    model: &str,
    ultracode: bool,
) -> EffortCommandResult {
    if is_ultracode_active(model, app_state_effort, ultracode) {
        return EffortCommandResult::message(
            "Current effort level: ultracode (xhigh + dynamic workflow orchestration; this session only)",
        );
    }
    let effective_value = match get_effort_env_override() {
        Some(None) => None,
        Some(Some(value)) => Some(value),
        None => (!is_launch_effort_pinned(model))
            .then(|| app_state_effort.cloned())
            .flatten(),
    };
    let Some(effective_value) = effective_value else {
        let level = get_displayed_effort_level(model, app_state_effort);
        return EffortCommandResult::message(format!("Effort level: auto (currently {level})"));
    };
    let description = get_effort_value_description(&effective_value);
    EffortCommandResult::message(format!(
        "Current effort level: {} ({description})",
        effective_value.as_str()
    ))
}

/// Maps to: CC `commands/effort/effort.tsx:86-113::unsetEffortLevel`.
fn unset_effort_level() -> EffortCommandResult {
    if let Err(error) = update_effort_setting(None) {
        return EffortCommandResult::message(format!("Failed to set effort level: {error}"));
    }

    unpin_launch_effort();
    if matches!(get_effort_env_override(), Some(Some(_))) {
        let env_raw = crate::utils::process_env::var("CLAUDE_CODE_EFFORT_LEVEL").unwrap_or_default();
        return EffortCommandResult::with_update(
            format!(
                "Cleared effort from settings, but CLAUDE_CODE_EFFORT_LEVEL={env_raw} still controls this session"
            ),
            None,
        );
    }
    EffortCommandResult::with_update("Effort level set to auto", None)
}

/// Maps to: CC `commands/effort/effort.tsx#setUltracodeEffort` (`Csm`).
fn set_ultracode_effort(model: &str) -> EffortCommandResult {
    if !is_ultracode_available(None) {
        return EffortCommandResult::message(format!(
            "Ultracode needs dynamic workflows enabled (see /config). Valid options are: {}",
            get_valid_options_list(model)
        ));
    }
    if model_supports_xhigh_effort(model) && !is_effort_within_org_limit("xhigh", model) {
        return EffortCommandResult::message(format!(
            "Ultracode runs at xhigh effort, which is restricted by your organization for {model}. Valid options are: {}",
            get_valid_options_list(model)
        ));
    }
    if !model_supports_xhigh_effort(model) {
        return EffortCommandResult::message(format!(
            "Ultracode runs at xhigh effort, which {model} doesn't support — switch to an xhigh-capable model ({XHIGH_CAPABLE_MODELS}). Valid options are: {}",
            get_valid_options_list(model)
        ));
    }
    unpin_launch_effort();
    let env_conflicts = match get_effort_env_override() {
        Some(Some(EffortValue::Named(level))) => level != "xhigh",
        Some(Some(_)) | Some(None) => true,
        None => false,
    };
    let value = EffortValue::Named("xhigh".to_string());
    if env_conflicts {
        let env_raw = crate::utils::process_env::var("CLAUDE_CODE_EFFORT_LEVEL").unwrap_or_default();
        return EffortCommandResult::with_ultracode(
            format!(
                "CLAUDE_CODE_EFFORT_LEVEL={env_raw} overrides effort this session — clear it and ultracode takes over"
            ),
            value,
        );
    }
    EffortCommandResult::with_ultracode(
        "Set effort level to ultracode (this session only): xhigh + dynamic workflow orchestration",
        value,
    )
}

/// Maps to: CC `commands/effort/effort.tsx#executeEffort` (`fdr`).
pub fn execute_effort(args: &str) -> EffortCommandResult {
    execute_effort_for_model(args, &current_model_fallback())
}

pub fn execute_effort_for_model(args: &str, model: &str) -> EffortCommandResult {
    let normalized = args.to_lowercase();
    if matches!(normalized.as_str(), "auto" | "unset") {
        return unset_effort_level();
    }
    if normalized == "ultracode" {
        return set_ultracode_effort(model);
    }
    let Some(level) = parse_extended_effort_level(args) else {
        return EffortCommandResult::message(format!(
            "Invalid argument: {args}. Valid options are: {}",
            get_valid_options_list(model)
        ));
    };
    set_effort_value(EffortValue::Named(level.to_string()), model)
}

fn current_model_fallback() -> String {
    crate::utils::model::model::get_main_loop_model()
}

fn current_effort_and_model(context: &ToolUseContext) -> (Option<EffortValue>, String, bool) {
    if let Some(state) = context.get_app_state() {
        let model = crate::hooks::use_main_loop_model::use_main_loop_model(
            state.main_loop_model.as_deref(),
            state.main_loop_model_for_session.as_deref(),
        );
        return (state.effort_value.clone(), model, state.ultracode);
    }
    let model = context
        .main_loop_model
        .clone()
        .unwrap_or_else(|| crate::hooks::use_main_loop_model::use_main_loop_model(None, None));
    (context.effort_value.clone(), model, false)
}

/// Maps to the state application in CC
/// `commands/effort/effort.tsx:142-161::ApplyEffortAndClose`.
fn apply_effort_and_close(
    result: EffortCommandResult,
    context: &ToolUseContext,
) -> EffortCommandResult {
    apply_effort_update(&result, |value, ultracode| {
        context.set_app_state(|state| {
            state.effort_value = value;
            state.ultracode = ultracode;
        });
    });
    result
}

fn apply_effort_update(
    result: &EffortCommandResult,
    apply: impl FnOnce(Option<EffortValue>, bool),
) {
    if let Some(value) = result.effort_update.clone() {
        apply(value, result.ultracode);
    }
}

/// Apply an `/effort` args dispatch onto a live [`AppStore`].
/// Maps to: CC `applyEffortCommand` (`$8o`).
pub fn apply_effort_command(args: &str, store: &AppStore) -> String {
    let model = {
        let state = store.get();
        crate::hooks::use_main_loop_model::use_main_loop_model(
            state.main_loop_model.as_deref(),
            state.main_loop_model_for_session.as_deref(),
        )
    };
    let result = execute_effort_for_model(args, &model);
    apply_effort_update(&result, |value, ultracode| {
        store.replace_with(|state| {
            state.effort_value = value;
            state.ultracode = ultracode;
        });
    });
    result.message
}

/// Maps to CC `normalizeEffortPickerArg` (bsm). Outer None is invalid.
pub fn normalize_effort_picker_arg(args: &str, model: &str) -> Option<Option<EffortValue>> {
    match args.to_lowercase().as_str() {
        "auto" | "unset" => Some(None),
        "ultracode" if is_ultracode_available(Some(model)) => {
            Some(Some(EffortValue::Named("xhigh".into())))
        }
        _ => parse_extended_effort_level(args).map(|level| Some(EffortValue::Named(level.into()))),
    }
}

/// Maps to: CC `commands/effort/effort.tsx#call` (`Zsm`).
pub fn call(args: &str, context: &ToolUseContext) -> EffortCall {
    let args = args.trim();
    if COMMON_HELP_ARGS.contains(&args) {
        let (_, model, _) = current_effort_and_model(context);
        return EffortCall::Result(EffortCommandResult::message(usage_text(&model)));
    }
    if matches!(args, "current" | "status") {
        let (effort, model, ultracode) = current_effort_and_model(context);
        return EffortCall::Result(show_current_effort(effort.as_ref(), &model, ultracode));
    }
    if args.is_empty() {
        return EffortCall::OpenPicker;
    }
    let (effort, model, _) = current_effort_and_model(context);
    let acked = context
        .get_app_state()
        .map_or(-1, |state| state.cache_miss_acked_at_output_tokens);
    if normalize_effort_picker_arg(args, &model).is_some_and(|value| {
        should_confirm_effort_cache_break(
            value.as_ref(),
            effort.as_ref(),
            &model,
            acked,
            !context.messages.is_empty(),
        )
    }) {
        return EffortCall::ConfirmArgs(args.to_string());
    }
    EffortCall::Result(apply_effort_and_close(
        execute_effort_for_model(args, &model),
        context,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    struct EnvGuard {
        config_dir: Option<EnvVarGuard>,
        effort: Option<EnvVarGuard>,
        root: std::path::PathBuf,
    }

    impl EnvGuard {
        fn isolated() -> Self {
            let root = std::env::temp_dir()
                .join(format!("cometix-effort-command-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            Self {
                config_dir: Some(EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root)),
                effort: Some(EnvVarGuard::unset("CLAUDE_CODE_EFFORT_LEVEL")),
                root,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            drop(self.config_dir.take());
            drop(self.effort.take());
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn named(value: &str) -> EffortValue {
        EffortValue::Named(value.to_string())
    }

    #[test]
    fn show_current_effort_matches_official_env_and_auto_messages() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::isolated();

        assert_eq!(
            show_current_effort(None, "claude-opus-4-6", false).message,
            "Effort level: auto (currently high)"
        );
        assert_eq!(
            show_current_effort(Some(&named("medium")), "claude-opus-4-6", false).message,
            "Current effort level: medium (Balanced approach with standard implementation and testing)"
        );
        crate::utils::process_env::set("CLAUDE_CODE_EFFORT_LEVEL", "low");
        assert_eq!(
            show_current_effort(Some(&named("high")), "claude-opus-4-6", false).message,
            "Current effort level: low (Quick, straightforward implementation with minimal overhead)"
        );
        crate::utils::process_env::set("CLAUDE_CODE_EFFORT_LEVEL", "auto");
        assert_eq!(
            show_current_effort(Some(&named("high")), "claude-opus-4-6", false).message,
            "Effort level: auto (currently high)"
        );
    }

    #[test]
    fn execute_effort_matches_official_persistence_session_and_unset_paths() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let guard = EnvGuard::isolated();

        let low = execute_effort("LOW");
        assert_eq!(
            low,
            EffortCommandResult::with_update(
                "Set effort level to low (saved as your default for new sessions): Quick, straightforward implementation with minimal overhead",
                Some(named("low")),
            )
        );
        let settings_path = guard.root.join("settings.json");
        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        assert_eq!(saved["effortLevel"], "low");

        // @cometix: persist `max` as a production effortLevel (CC 2.1.88: ant-only).
        let max = execute_effort("max");
        assert_eq!(
            max.message,
            "Set effort level to max (saved as your default for new sessions): Maximum capability with deepest reasoning (Fable 5, Opus 4.6+, Sonnet 4.6+)"
        );
        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        assert_eq!(saved["effortLevel"], "max");

        let xhigh = execute_effort_for_model("xhigh", "claude-opus-4-7");
        assert_eq!(
            xhigh.message,
            "Set effort level to xhigh (saved as your default for new sessions): Extended reasoning with thorough analysis (Fable 5, Opus 4.7+, Sonnet 5)"
        );
        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        assert_eq!(saved["effortLevel"], "xhigh");

        assert_eq!(
            execute_effort_for_model("invalid", "claude-opus-4-6").message,
            "Invalid argument: invalid. Valid options are: low, medium, high, xhigh, max, auto"
        );
        assert_eq!(
            execute_effort_for_model("ultracode", "claude-opus-4-7").message,
            "Ultracode needs dynamic workflows enabled (see /config). Valid options are: low, medium, high, xhigh, max, auto"
        );
        assert_eq!(execute_effort("auto").message, "Effort level set to auto");
        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(settings_path).unwrap()).unwrap();
        assert!(!saved.as_object().unwrap().contains_key("effortLevel"));
    }

    #[test]
    fn execute_effort_matches_official_conflicting_env_messages() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::isolated();
        crate::utils::process_env::set("CLAUDE_CODE_EFFORT_LEVEL", "low");

        assert_eq!(
            execute_effort("high").message,
            "CLAUDE_CODE_EFFORT_LEVEL=low overrides this session — clear it and high takes over"
        );
        // @cometix: persist `max` as a production effortLevel (CC 2.1.88: ant-only).
        assert_eq!(
            execute_effort("max").message,
            "CLAUDE_CODE_EFFORT_LEVEL=low overrides this session — clear it and max takes over"
        );
        assert_eq!(
            execute_effort("auto").message,
            "Cleared effort from settings, but CLAUDE_CODE_EFFORT_LEVEL=low still controls this session"
        );
    }

    #[test]
    fn call_matches_official_help_current_and_app_state_application() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::isolated();
        let store = crate::state::store::AppStore::new(
            crate::state::app_state_store::AppState::default(),
            None,
        );
        let context = ToolUseContext::default().with_app_store(store.clone());

        assert!(matches!(call("", &context), EffortCall::OpenPicker));
        let help = call(" help ", &context).into_result().unwrap();
        assert!(help.message.contains("xhigh"));
        assert!(help.message.contains("ultracode") || help.message.contains("auto"));
        assert_eq!(
            call("current", &context).into_result().unwrap().message,
            "Effort level: auto (currently high)"
        );
        // @cometix: persist `max` as a production effortLevel (CC 2.1.88: ant-only).
        let result = call("max", &context).into_result().unwrap();
        assert_eq!(
            result.message,
            "Set effort level to max (saved as your default for new sessions): Maximum capability with deepest reasoning (Fable 5, Opus 4.6+, Sonnet 4.6+)"
        );
        assert_eq!(store.get().effort_value, Some(named("max")));
        assert!(!store.get().ultracode);

        store.replace_with(|state| {
            state.main_loop_model = Some("claude-opus-4-7".to_string());
        });
        let before = store.get().effort_value.clone();
        let ultra_message = apply_effort_command("ultracode", &store);
        assert_eq!(
            ultra_message,
            "Ultracode needs dynamic workflows enabled (see /config). Valid options are: low, medium, high, xhigh, max, auto"
        );
        assert_eq!(store.get().effort_value, before);
        assert!(!store.get().ultracode);
    }
}
