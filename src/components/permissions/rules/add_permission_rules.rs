//! Maps to: CC `components/permissions/rules/AddPermissionRules.tsx`.
//!
//! Save callbacks retain the source apply/persist/set/detect/notify sequence.
//! Blocking persistence runs off-frame with captured, owned source callbacks.

use super::permission_rule_description::PermissionRuleDescription;
use crate::components::custom_select::{
    Select, SelectInputOptionMeta, SelectLayout, SelectOptionData, UseSelectInputOptions,
    UseSelectStateProps, use_select_input, use_select_state,
};
use crate::components::design_system::dialog::Dialog;
use crate::tool::ToolPermissionContext;
use crate::types::permissions::{
    PermissionBehavior, PermissionRule, PermissionRuleValue, PermissionUpdate,
    PermissionUpdateDestination,
};
use crate::utils::permissions::permission_rule_parser::permission_rule_value_to_string;
use crate::utils::permissions::permission_update::{
    apply_permission_update, persist_permission_update,
};
use crate::utils::permissions::shadowed_rule_detection::{
    DetectUnreachableRulesOptions, UnreachableRule, detect_unreachable_rules,
};
use crate::utils::settings::{SettingSource, get_relative_settings_file_path_for_source};
use iocraft::prelude::*;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddPermissionRulesOutcome {
    pub rules: Vec<PermissionRule>,
    pub unreachable: Option<Vec<UnreachableRule>>,
}

/// Maps to: CC `optionForPermissionSaveDestination(...)`.
pub fn option_for_permission_save_destination(
    save_destination: SettingSource,
) -> Option<SelectOptionData> {
    let (label, description, value) = match save_destination {
        SettingSource::Local => (
            "Project settings (local)",
            format!(
                "Saved in {}",
                get_relative_settings_file_path_for_source(save_destination)
            ),
            "localSettings",
        ),
        SettingSource::Project => (
            "Project settings",
            format!(
                "Checked in at {}",
                get_relative_settings_file_path_for_source(save_destination)
            ),
            "projectSettings",
        ),
        SettingSource::User => (
            "User settings",
            "Saved in at ~/.claude/settings.json".to_string(),
            "userSettings",
        ),
        _ => return None,
    };
    Some(SelectOptionData {
        label: label.to_string(),
        description: Some(description.to_string()),
        dim_description: true,
        value: value.to_string(),
        disabled: false,
        input: None,
    })
}

#[derive(Default, Props)]
pub struct AddPermissionRulesProps<'a> {
    pub rule_values: Vec<PermissionRuleValue>,
    pub rule_behavior: Option<PermissionBehavior>,
    /// Maps to: CC `onAddRules` / `setToolPermissionContext`. Owned callbacks
    /// preserve the event-started persistence continuation across unmount. Local
    /// parent state must be delivered via its retained inbox, never cross-thread.
    pub on_add_rules: Option<Arc<dyn Fn(AddPermissionRulesOutcome) + Send + Sync>>,
    pub initial_context: Arc<ToolPermissionContext>,
    pub set_tool_permission_context: Option<Arc<dyn Fn(Arc<ToolPermissionContext>) + Send + Sync>>,
    /// Maps to: CC `onCancel` (`onSelect` 'cancel' branch,
    /// AddPermissionRules.tsx:77-79).
    pub on_cancel: HandlerMut<'a, ()>,
}

/// Maps to: CC `AddPermissionRules` render path
/// (AddPermissionRules.tsx:65-165).
#[component]
pub fn AddPermissionRules<'a>(
    props: &mut AddPermissionRulesProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    let behavior = props.rule_behavior.unwrap_or(PermissionBehavior::Allow);
    let mut pending_cancel = hooks.use_state(|| false);
    // CC onSelect has no await. Block further input while its synchronous
    // section is transported off-frame; this is not a new cancellable job.
    let mut saving = hooks.use_state(|| false);
    let completion = hooks.use_const(|| Arc::new(async_channel::unbounded::<()>()));
    let receiver = completion.1.clone();
    hooks.use_future(async move {
        while receiver.recv().await.is_ok() {
            saving.set(false);
        }
    });
    hooks.use_propagated_terminal_events(move |event| {
        if saving.get() {
            event.stop_propagation();
        }
    });
    let options = crate::utils::settings::constants::SOURCES
        .into_iter()
        .filter_map(option_for_permission_save_destination)
        .collect::<Vec<_>>();
    // Maps to: CC AddPermissionRules.tsx:161 — bare
    // `<Select options={allOptions} onChange={onSelect}/>` inside the Dialog
    // (no onCancel / edge callbacks; visibleOptionCount defaults to 5,
    // select.tsx:207).
    let state = use_select_state(
        &mut hooks,
        UseSelectStateProps {
            visible_option_count: Some(5),
            values: options.iter().map(|option| option.value.clone()).collect(),
            default_value: None,
            focus_value: None,
        },
    );
    let events = use_select_input(
        &mut hooks,
        state,
        UseSelectInputOptions {
            is_disabled: saving.get(),
            option_metas: options
                .iter()
                .map(|option| SelectInputOptionMeta {
                    value: option.value.clone(),
                    disabled: option.disabled,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        },
    );

    // Maps to: CC AddPermissionRules.tsx:75-137 onSelect. The complete
    // synchronous source sequence stays together on the off-frame worker.
    if let Some(value) = events.take_accepted().filter(|_| !saving.get()) {
        if value == "cancel" {
            pending_cancel.set(true);
        } else if let Some(destination) = options
            .iter()
            .find(|option| option.value == value)
            .and_then(|option| match option.value.as_str() {
                "localSettings" => Some(PermissionUpdateDestination::LocalSettings),
                "projectSettings" => Some(PermissionUpdateDestination::ProjectSettings),
                "userSettings" => Some(PermissionUpdateDestination::UserSettings),
                _ => None,
            })
        {
            saving.set(true);
            let completed = completion.0.clone();
            let rule_values = props.rule_values.clone();
            let initial_context = props.initial_context.clone();
            let set_context = props.set_tool_permission_context.clone();
            let on_add_rules = props.on_add_rules.clone();
            tokio::task::spawn_blocking(move || {
                let update = PermissionUpdate::AddRules {
                    destination,
                    behavior,
                    rules: rule_values.clone(),
                };
                let updated_context = Arc::new(apply_permission_update(&initial_context, &update));
                if let Err(error) = persist_permission_update(&update) {
                    crate::utils::log::log_error(crate::utils::log::LogError::new(
                        error.to_string(),
                    ));
                    let _ = completed.try_send(());
                    return;
                }
                if let Some(set_context) = set_context {
                    set_context(updated_context.clone());
                }
                let source = crate::utils::permissions::permission_update::permission_rule_source_for_destination(destination);
                let rules = rule_values
                    .iter()
                    .cloned()
                    .map(|rule_value| PermissionRule {
                        rule_value,
                        rule_behavior: behavior,
                        source,
                    })
                    .collect();
                let sandbox_auto_allow_enabled = crate::utils::sandbox::sandbox_adapter::is_sandboxing_enabled()
                    && crate::utils::sandbox::sandbox_adapter::is_auto_allow_bash_if_sandboxed_enabled(&crate::utils::settings::get_initial_settings());
                let unreachable: Vec<_> = detect_unreachable_rules(
                    &updated_context,
                    DetectUnreachableRulesOptions {
                        sandbox_auto_allow_enabled,
                    },
                )
                .into_iter()
                .filter(|unreachable| {
                    rule_values.iter().any(|value| {
                        value.tool_name == unreachable.rule.rule_value.tool_name
                            && value.rule_content == unreachable.rule.rule_value.rule_content
                    })
                })
                .collect();
                if let Some(on_add_rules) = on_add_rules {
                    on_add_rules(AddPermissionRulesOutcome {
                        rules,
                        unreachable: (!unreachable.is_empty()).then_some(unreachable),
                    });
                }
                let _ = completed.try_send(());
            });
        }
    }
    if pending_cancel.get() {
        pending_cancel.set(false);
        if !saving.get() {
            (props.on_cancel)(());
        }
    }

    let navigation = state.navigation.snapshot();
    let focused_index = navigation.focused_index().unwrap_or(0);
    let behavior_label = match behavior {
        PermissionBehavior::Allow => "allow",
        PermissionBehavior::Deny => "deny",
        PermissionBehavior::Ask => "ask",
    };
    let noun = if props.rule_values.len() == 1 {
        "rule"
    } else {
        "rules"
    };
    let title = format!("Add {behavior_label} permission {noun}");
    element! {
        Dialog(title, color: Some(theme.permission), is_cancel_active: Some(!saving.get()), on_cancel: move |_| { if !saving.get() { pending_cancel.set(true); } }) {
            View(flex_direction: FlexDirection::Column, padding_left: 2u32, padding_right: 2u32) {
                #(props.rule_values.iter().map(|rule_value| element! {
                    View(key: permission_rule_value_to_string(rule_value), flex_direction: FlexDirection::Column) {
                        Text(content: permission_rule_value_to_string(rule_value), weight: Weight::Bold)
                        PermissionRuleDescription(rule_value: Some(rule_value.clone()))
                    }
                }))
            }
            View(flex_direction: FlexDirection::Column, margin_top: 1u32, margin_bottom: 1u32) {
                Text(content: if props.rule_values.len() == 1 { "Where should this rule be saved?".to_string() } else { "Where should these rules be saved?".to_string() })
                Select(
                    options: options.clone(),
                    focused_index: focused_index,
                    visible_option_count: navigation.visible_option_count,
                    visible_from_index: navigation.visible_from_index,
                    layout: SelectLayout::Compact,
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn add_permission_rules_options_match_official_destinations() {
        let options = [
            SettingSource::Local,
            SettingSource::Project,
            SettingSource::User,
        ]
        .into_iter()
        .filter_map(option_for_permission_save_destination)
        .collect::<Vec<_>>();
        assert_eq!(
            options
                .iter()
                .map(|option| option.value.as_str())
                .collect::<Vec<_>>(),
            vec!["localSettings", "projectSettings", "userSettings"]
        );
        assert_eq!(options[0].label, "Project settings (local)");
        assert_eq!(
            options[0].description.as_deref(),
            Some("Saved in .claude/settings.local.json")
        );
    }

    #[test]
    fn add_permission_rules_renders_shared_select_with_pointer_and_indexes() {
        let text = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                AddPermissionRules(
                    rule_values: vec![PermissionRuleValue::new(
                        "Bash",
                        Some("cargo test:*".to_string()),
                    )],
                )
            }
        }
        .render(Some(140))
        .to_string();
        assert!(
            text.contains("Where should this rule be saved?"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("❯ 1. Project settings (local)"),
            "shared Select should render the focused pointer and index; canvas=\n{text}"
        );
        assert!(text.contains("3. User settings"), "canvas=\n{text}");
    }

    #[tokio::test]
    async fn add_permission_rules_matches_official_persist_set_detect_notify_order_and_single_sync_action()
     {
        use crate::types::permissions::PermissionRuleSource;
        use futures::StreamExt;
        use std::time::Duration;
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("cc-rule-save-{}", uuid::Uuid::new_v4()));
        let workdir = root.join("workspace");
        std::fs::create_dir_all(&workdir).unwrap();
        let prior_cwd = crate::bootstrap::state::get_original_cwd();
        let prior_disabled = crate::bootstrap::state::is_session_persistence_disabled();
        let _write = EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1");
        let _config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", root.join("config"));
        let _managed = EnvVarGuard::set("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &root);
        let _skip = EnvVarGuard::unset("CLAUDE_CODE_SKIP_PROMPT_HISTORY");
        crate::bootstrap::state::set_original_cwd(&workdir);
        crate::bootstrap::state::set_session_persistence_disabled(false);
        crate::utils::settings::settings_cache::reset_settings_cache();
        let mut context = ToolPermissionContext::default();
        context.always_allow_rules.insert(
            PermissionRuleSource::UserSettings,
            vec![PermissionRuleValue::new("Read", Some("/old".to_string()))],
        );
        context.always_deny_rules.insert(
            PermissionRuleSource::UserSettings,
            vec![
                PermissionRuleValue::new("Read", None),
                PermissionRuleValue::new("Bash", None),
            ],
        );
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let setter_order = order.clone();
        let result_order = order.clone();
        let (completed, results) = async_channel::unbounded();
        let settings_file = workdir.join(".claude/settings.local.json");
        let (setter_entered, entered) = async_channel::bounded(1);
        let (release, waiting) = std::sync::mpsc::channel();
        let waiting = std::sync::Mutex::new(waiting);
        let cancelled = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cancel_count = cancelled.clone();
        let setter: Arc<dyn Fn(Arc<ToolPermissionContext>) + Send + Sync> =
            Arc::new(move |updated| {
                // The exact persisted file must exist before the source setter.
                let saved: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&settings_file).unwrap()).unwrap();
                assert_eq!(
                    saved["permissions"]["allow"],
                    serde_json::json!(["Bash(ls:*)"])
                );
                assert_eq!(
                    updated.always_allow_rules[&PermissionRuleSource::LocalSettings][0].tool_name,
                    "Bash"
                );
                setter_order.lock().unwrap().push("set");
                setter_entered.try_send(()).unwrap();
                // Hold the synchronous source segment in a real callback while
                // the retained UI processes a burst; no production delay seam.
                waiting
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap();
            });
        let callback: Arc<dyn Fn(AddPermissionRulesOutcome) + Send + Sync> =
            Arc::new(move |outcome| {
                result_order.lock().unwrap().push("notify");
                let _ = completed.try_send(outcome);
            });
        let mut app = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                ContextProvider(value: Context::owned(crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings())) {
                    AddPermissionRules(rule_values: vec![PermissionRuleValue::new("Bash", Some("ls:*".to_string()))],
                        initial_context: Arc::new(context), set_tool_permission_context: Some(setter), on_add_rules: Some(callback),
                        on_cancel: move |_| { cancel_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst); })
                }
            }
        };
        let (keys, events) = async_channel::unbounded();
        let mut frames =
            Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(120, 24),
            ));
        let deadline = futures_timer::Delay::new(Duration::from_secs(3));
        tokio::pin!(deadline);
        let burst_keys = keys.clone();
        let burst = async move {
            entered.recv().await.unwrap();
            for code in [KeyCode::Char('2'), KeyCode::Enter, KeyCode::Esc] {
                burst_keys
                    .send(TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code)))
                    .await
                    .unwrap();
            }
            futures_timer::Delay::new(Duration::from_millis(50)).await;
            release.send(()).unwrap();
        };
        tokio::pin!(burst);
        let mut burst_finished = false;
        let mut sent = false;
        let outcome = loop {
            tokio::select! {
                _ = &mut deadline => break None,
                _ = &mut burst, if !burst_finished => { burst_finished = true; },
                result = results.recv() => break result.ok(),
                frame = frames.next() => {
                    let Some(frame) = frame else { break None; };
                    if !sent && frame.to_string().contains("❯ 1. Project settings (local)") {
                        keys.send(TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Enter))).await.unwrap(); sent = true;
                    }
                }
            }
        };
        let outcome = outcome.expect("source save callback did not complete");
        assert!(
            burst_finished,
            "the repeated-input burst must finish before notification"
        );
        assert_eq!(
            cancelled.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "sync save cannot become cancellable while transported off-frame"
        );
        assert_eq!(*order.lock().unwrap(), vec!["set", "notify"]);
        assert_eq!(
            outcome.rules,
            vec![PermissionRule {
                rule_value: PermissionRuleValue::new("Bash", Some("ls:*".to_string())),
                rule_behavior: PermissionBehavior::Allow,
                source: PermissionRuleSource::LocalSettings
            }]
        );
        let unreachable = outcome.unreachable.expect("new Bash rule is denied");
        assert_eq!(
            unreachable.len(),
            1,
            "pre-existing unreachable Read must be filtered out"
        );
        assert_eq!(unreachable[0].rule.rule_value.tool_name, "Bash");
        assert!(!workdir.join(".claude/settings.json").exists());
        assert!(!root.join("config/settings.json").exists());
        drop(frames);
        crate::bootstrap::state::set_original_cwd(prior_cwd);
        crate::bootstrap::state::set_session_persistence_disabled(prior_disabled);
        crate::utils::settings::settings_cache::reset_settings_cache();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn add_permission_rules_matches_official_dialog_escape_without_save() {
        use futures::{StreamExt, stream};
        use std::time::Duration;
        let cancellations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cancelled = cancellations.clone();
        let setter: Arc<dyn Fn(Arc<ToolPermissionContext>) + Send + Sync> =
            Arc::new(|_| panic!("cancel must not set permissions"));
        let callback: Arc<dyn Fn(AddPermissionRulesOutcome) + Send + Sync> =
            Arc::new(|_| panic!("cancel must not report saved rules"));
        let mut app = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                ContextProvider(value: Context::owned(crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings())) {
                    AddPermissionRules(on_cancel: move |_| { cancelled.fetch_add(1, std::sync::atomic::Ordering::SeqCst); }, set_tool_permission_context: Some(setter), on_add_rules: Some(callback))
                }
            }
        };
        let events = stream::once(async {
            futures_timer::Delay::new(Duration::from_millis(20)).await;
            TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Esc))
        })
        .chain(stream::pending());
        let mut frames =
            Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(100, 20),
            ));
        let deadline = futures_timer::Delay::new(Duration::from_secs(1));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                frame = frames.next() => { if frame.is_none() || cancellations.load(std::sync::atomic::Ordering::SeqCst) != 0 { break; } }
            }
        }
        assert_eq!(cancellations.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
