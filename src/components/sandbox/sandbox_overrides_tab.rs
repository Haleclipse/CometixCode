//! Maps to: CC `components/sandbox/SandboxOverridesTab.tsx`.
//!
//! With the sandbox off, or its override settings locked by policy, the tab
//! is static text and takes no keys. Otherwise `OverridesSelect` owns the
//! Select, writes `allowUnsandboxedCommands` and reports through
//! `on_complete`.

use crate::components::custom_select::select::SelectOptionLabel;
use crate::components::custom_select::{
    Select, SelectInputOptionMeta, SelectLayout, SelectOptionData, UseSelectInputOptions,
    UseSelectStateProps, use_select_input, use_select_state,
};
use crate::components::design_system::tabs::use_tab_header_focus;
use crate::utils::sandbox::sandbox_adapter::{SandboxSettingsUpdate, set_sandbox_settings};
use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverrideMode {
    Open,
    Closed,
}

impl OverrideMode {
    pub fn value(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
        }
    }
}

/// Maps to: CC `SandboxOverridesTab.tsx:70-85`. The labels carry the
/// `(current)` suffix as text; `current_indicator_labels` renders it green.
pub fn sandbox_override_options(current_mode: OverrideMode) -> Vec<SelectOptionData> {
    [
        (OverrideMode::Open, "Allow unsandboxed fallback"),
        (OverrideMode::Closed, "Strict sandbox mode"),
    ]
    .into_iter()
    .map(|(mode, label)| SelectOptionData {
        label: if mode == current_mode {
            format!("{label} (current)")
        } else {
            label.to_string()
        },
        description: None,
        dim_description: true,
        value: mode.value().to_string(),
        disabled: false,
        input: None,
    })
    .collect()
}

/// Maps to: CC `SandboxOverridesTab.tsx:68`
/// `color('success', theme)('(current)')`, a coloured span inside the
/// current option's label string.
fn current_indicator_labels(
    options: &[SelectOptionData],
    current_value: &str,
    theme: &Theme,
) -> BTreeMap<String, SelectOptionLabel> {
    options
        .iter()
        .filter(|option| option.value == current_value)
        .map(|option| {
            let base = option.label.trim_end_matches(" (current)");
            let mut indicator = StyledSegment::new("(current)");
            indicator.styles.color = Some(theme.success);
            (
                option.value.clone(),
                vec![StyledSegment::new(format!("{base} ")), indicator],
            )
        })
        .collect()
}

#[derive(Default, Props)]
pub struct SandboxOverridesTabProps {
    /// CC `SandboxManager.isSandboxingEnabled()` (:18).
    pub is_enabled: bool,
    /// CC `SandboxManager.areSandboxSettingsLockedByPolicy()` (:19).
    pub is_locked: bool,
    /// CC `SandboxManager.areUnsandboxedCommandsAllowed()` (:20).
    pub current_allow_unsandboxed: bool,
    /// CC `onComplete(result)`; `None` is `onComplete(undefined, { display:
    /// 'skip' })`.
    pub on_complete: Handler<Option<String>>,
}

/// Maps to: CC `SandboxOverridesTab.tsx:17-57`. SandboxSettings reads the
/// three SandboxManager values and passes them in.
#[component]
pub fn SandboxOverridesTab(
    props: &SandboxOverridesTabProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = *hooks.use_context::<Theme>();

    // CC :22-30.
    if !props.is_enabled {
        return element! {
            View(flex_direction: FlexDirection::Column, padding_top: 1u32, padding_bottom: 1u32) {
                Text(content: "Sandbox is not enabled. Enable sandbox to configure override settings.".to_string(), color: theme.subtle, wrap: TextWrap::Wrap)
            }
        }
        .into_any();
    }

    // CC :32-49: locked by a higher-priority source, the tab is text only.
    if props.is_locked {
        return element! {
            View(flex_direction: FlexDirection::Column, padding_top: 1u32, padding_bottom: 1u32) {
                Text(content: "Override settings are managed by a higher-priority configuration and cannot be changed locally.".to_string(), color: theme.subtle, wrap: TextWrap::Wrap)
                View(margin_top: 1u32) {
                    Text(content: format!("Current setting: {}", if props.current_allow_unsandboxed { "Allow unsandboxed fallback" } else { "Strict sandbox mode" }), color: theme.inactive, wrap: TextWrap::Wrap)
                }
            }
        }
        .into_any();
    }

    // CC :51-56.
    element! {
        OverridesSelect(
            current_mode: Some(if props.current_allow_unsandboxed {
                OverrideMode::Open
            } else {
                OverrideMode::Closed
            }),
            on_complete: props.on_complete.clone(),
        )
    }
    .into_any()
}

#[derive(Default, Props)]
struct OverridesSelectProps {
    current_mode: Option<OverrideMode>,
    on_complete: Handler<Option<String>>,
}

/// Maps to: CC `SandboxOverridesTab.tsx:59-139`. A separate component, as in
/// CC, so the header-focus opt-in only runs where the Select renders: above
/// the early returns, ↓ would blur the header onto static text with no way
/// back.
#[component]
fn OverridesSelect(
    props: &OverridesSelectProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = *hooks.use_context::<Theme>();
    let focus = use_tab_header_focus(&mut hooks);
    let current_mode = props.current_mode.unwrap_or(OverrideMode::Closed);
    let options = sandbox_override_options(current_mode);
    let option_labels = current_indicator_labels(&options, current_mode.value(), &theme);
    // CC :107-113 `<Select options onChange onCancel onUpFromFirstItem
    // isDisabled={headerFocused}>`: the default five-row viewport.
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
            is_disabled: focus.header_focused,
            has_on_cancel: true,
            has_on_up_from_first_item: true,
            option_metas: options
                .iter()
                .map(|option| SelectInputOptionMeta {
                    value: option.value.clone(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        },
    );
    // CC :87-100 `handleSelect`.
    if let Some(value) = events.take_accepted() {
        let allow = value == OverrideMode::Open.value();
        // `setSandboxSettings` logs a failed write and resolves anyway.
        set_sandbox_settings(SandboxSettingsUpdate {
            enabled: None,
            auto_allow_bash_if_sandboxed: None,
            allow_unsandboxed_commands: Some(allow),
        });
        let message = if allow {
            "✓ Unsandboxed fallback allowed - commands can run outside sandbox when necessary"
        } else {
            "✓ Strict sandbox mode - all commands must run in sandbox or be excluded via the `excludedCommands` option"
        };
        (props.on_complete)(Some(message.to_string()));
    }
    if events.take_cancelled() {
        (props.on_complete)(None);
    }
    if events.take_up_from_first_item() {
        focus.focus_header();
    }
    let navigation = state.navigation.snapshot();

    let heading = |text: &str| {
        let mut segment = StyledSegment::new(text);
        segment.styles.bold = Some(true);
        segment.styles.color = Some(theme.inactive);
        segment
    };
    let body = |text: &str| {
        let mut segment = StyledSegment::new(text);
        segment.styles.color = Some(theme.inactive);
        segment
    };
    let mut link = link_segment(
        "https://code.claude.com/docs/en/sandboxing#configure-sandboxing".to_string(),
        Some("code.claude.com/docs/en/sandboxing#configure-sandboxing".to_string()),
        None,
        None,
    );
    link.styles.color = Some(theme.inactive);
    let allow_text = vec![
        heading("Allow unsandboxed fallback:"),
        body(" When a command fails due to sandbox restrictions, Claude can retry with dangerouslyDisableSandbox to run outside the sandbox (falling back to default permissions)."),
    ];
    let strict_text = vec![
        heading("Strict sandbox mode:"),
        body(" All bash commands invoked by the model must run in the sandbox unless they are explicitly listed in excludedCommands."),
    ];
    let learn_more = vec![body("Learn more: "), link];

    // CC :102-138.
    element! {
        View(flex_direction: FlexDirection::Column, padding_top: 1u32, padding_bottom: 1u32) {
            View(margin_bottom: 1u32) {
                Text(content: "Configure Overrides:".to_string(), weight: Weight::Bold, wrap: TextWrap::Wrap)
            }
            Select(
                is_disabled: focus.header_focused,
                options: options,
                option_labels: option_labels,
                selected_value: state.committed_value(),
                focused_index: navigation.focused_index().unwrap_or(0),
                visible_option_count: navigation.visible_option_count,
                visible_from_index: navigation.visible_from_index,
                layout: SelectLayout::Compact,
            )
            View(flex_direction: FlexDirection::Column, margin_top: 1u32, gap: 1) {
                Text(segments: Some(allow_text), wrap: TextWrap::Wrap)
                Text(segments: Some(strict_text), wrap: TextWrap::Wrap)
                Text(segments: Some(learn_more), wrap: TextWrap::Wrap)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybindings::keybinding_context::KeybindingRuntime;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};
    use crate::utils::theme;
    use futures::StreamExt;
    use std::sync::{Arc, Mutex};

    type Results = Arc<Mutex<Vec<Option<String>>>>;

    #[derive(Default, Props)]
    struct OverridesHarnessProps {
        is_locked: bool,
        results: Option<Results>,
    }

    /// The tab, enabled, outside a Tabs (content focused, as CC's default
    /// TabsContext has it), and a line counting key presses so each key
    /// produces a frame.
    #[component]
    fn OverridesHarness(props: &OverridesHarnessProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let runtime = crate::keybindings::keybinding_provider_setup::use_keybinding_setup(
            &mut hooks,
            KeybindingRuntime::with_default_bindings(),
        );
        let results = props.results.clone().unwrap_or_default();
        let completed = results.clone();
        let mut keys = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Key(key) if key.kind == KeyEventKind::Press) {
                keys.set(keys.get() + 1);
            }
        });
        element! {
            ContextProvider(value: Context::owned(runtime)) {
                ContextProvider(value: Context::owned(*theme::current())) {
                    View(flex_direction: FlexDirection::Column) {
                        SandboxOverridesTab(
                            is_enabled: true,
                            is_locked: props.is_locked,
                            current_allow_unsandboxed: false,
                            on_complete: Handler::from(move |result: Option<String>| {
                                completed.lock().unwrap().push(result);
                            }),
                        )
                        Text(content: format!("keys={} pressed", keys.get()))
                    }
                }
            }
        }
    }

    /// Sends one key per frame; returns the last frame and the results.
    fn drive(is_locked: bool, keys_in_order: Vec<KeyCode>) -> (String, Vec<Option<String>>) {
        let results: Results = Arc::new(Mutex::new(Vec::new()));
        let echo = results.clone();
        let last = futures::executor::block_on(async move {
            let (keys, events) = async_channel::unbounded();
            let mut app = element!(OverridesHarness(is_locked: is_locked, results: Some(echo)));
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(120, 30),
            ));
            let mut sent = 0;
            let mut last = String::new();
            while let Some(canvas) = render_loop.next().await {
                last = canvas.to_string();
                if !last.contains(&format!("keys={sent} ")) {
                    continue;
                }
                let Some(code) = keys_in_order.get(sent) else {
                    break;
                };
                keys.send(TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, *code)))
                    .await
                    .unwrap();
                sent += 1;
            }
            last
        });
        let results = results.lock().unwrap().clone();
        (last, results)
    }

    #[test]
    fn sandbox_override_options_mark_current_like_official() {
        let options = sandbox_override_options(OverrideMode::Closed);
        assert_eq!(options[0].label, "Allow unsandboxed fallback");
        assert_eq!(options[1].label, "Strict sandbox mode (current)");
    }

    #[test]
    fn sandbox_overrides_disabled_branch_matches_official_copy() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                SandboxOverridesTab(is_enabled: false, is_locked: false, current_allow_unsandboxed: true)
            }
        }
        .render(Some(100))
        .to_string();
        assert!(text.contains("Sandbox is not enabled"), "canvas=\n{text}");
    }

    #[test]
    fn sandbox_overrides_locked_by_policy_is_text_only_and_writes_nothing() {
        // CC :32-49: the locked tab has no Select, so Enter chooses nothing
        // and allowUnsandboxedCommands is never written.
        let (last, results) = drive(true, vec![KeyCode::Enter, KeyCode::Down, KeyCode::Enter, KeyCode::F(12)]);
        assert!(last.contains("managed by a higher-priority configuration"), "{last}");
        assert!(last.contains("Current setting: Strict sandbox mode"), "{last}");
        assert!(!last.contains("Configure Overrides:"), "{last}");
        assert!(results.is_empty(), "{results:?}");
    }

    #[test]
    fn sandbox_overrides_select_owns_its_keys() {
        // CC :87-113: Enter writes the focused override (the first, "open")
        // and reports its message, which CC's never-rejecting
        // setSandboxSettings leaves in place whether or not the write lands;
        // Esc cancels with a skip. Writes stay off in this test.
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _no_write = EnvVarGuard::unset("COMETIX_WRITE_ENABLED");
        let (last, results) = drive(false, vec![KeyCode::Enter, KeyCode::Esc, KeyCode::F(12)]);
        assert!(last.contains("Configure Overrides:"), "{last}");
        assert_eq!(
            results,
            vec![
                Some("✓ Unsandboxed fallback allowed - commands can run outside sandbox when necessary".to_string()),
                None,
            ]
        );
    }
}
