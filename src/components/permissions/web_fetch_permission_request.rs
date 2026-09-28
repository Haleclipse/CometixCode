//! Maps to: CC
//! `components/permissions/WebFetchPermissionRequest/WebFetchPermissionRequest.tsx`.
//!
//! Ports the WebFetch-specific permission dialog boundary: URL/prompt preview,
//! domain-scoped allow option construction, and cancel/selection handling. The
//! network execution path remains owned by `tools/web_fetch_tool`.

use super::permission_dialog::PermissionDialog;
use super::permission_rule_explanation::{PermissionRuleExplanation, PermissionRuleToolType};
use super::worker_badge::WorkerBadgeProps;
use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::types::permissions::{
    PermissionBehavior, PermissionPromptChoice, PermissionPromptResponse,
    PermissionRequest as PermissionRequestData, PermissionUpdate, PermissionUpdateDestination,
};
use crate::utils::theme::Theme;
use iocraft::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WebFetchPermissionOptionValue {
    Yes,
    YesDontAskAgainDomain,
    No,
}

impl WebFetchPermissionOptionValue {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Yes => "yes",
            Self::YesDontAskAgainDomain => "yes-dont-ask-again-domain",
            Self::No => "no",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebFetchPermissionOption {
    pub label: String,
    pub value: WebFetchPermissionOptionValue,
}

impl WebFetchPermissionOption {
    fn select(label: impl Into<String>, value: WebFetchPermissionOptionValue) -> Self {
        Self {
            label: label.into(),
            value,
        }
    }

    pub fn to_select_option(&self) -> SelectOptionData {
        SelectOptionData {
            label: self.label.clone(),
            description: None,
            dim_description: true,
            value: self.value.as_str().to_string(),
            disabled: false,
            input: None,
        }
    }
}

#[derive(Default, Props)]
pub struct WebFetchPermissionRequestProps<'a> {
    pub request: Option<PermissionRequestData>,
    pub worker_badge: Option<WorkerBadgeProps>,
    pub show_always_allow_options: bool,
    /// Maps to: CC `PermissionRequestProps.verbose`
    /// (`WebFetchPermissionRequest.tsx:33`), consumed at `:120-129` as
    /// `WebFetchTool.renderToolUseMessage(input, { theme, verbose })`.
    /// `UI.tsx:17-20` returns the bare url unless verbose, in which case it
    /// returns `url: "…", prompt: "…"`.
    pub verbose: bool,
    pub on_select: HandlerMut<'a, WebFetchPermissionOptionValue>,
}

fn default_request() -> PermissionRequestData {
    PermissionRequestData {
        permission_result: None,
        id: String::new(),
        tool_use_id: String::new(),
        tool_name: "WebFetch".to_string(),
        mcp_info: None,
        decision_reason: None,
        description: String::new(),
        message: String::new(),
        input_summary: String::new(),
        input: serde_json::Value::Null,
        call_input: None,
        rule: crate::types::permissions::PermissionRuleValue::new("WebFetch", None),
        suggestions: Vec::new(),
        blocked_path: None,
        metadata: None,
        is_compound_command: false,
        mode: crate::types::permissions::PermissionMode::Default,
    }
}

/// Maps to: CC `inputToPermissionRuleContent(...)` in
/// `WebFetchPermissionRequest.tsx`.
pub fn web_fetch_input_to_permission_rule_content(input: &serde_json::Value) -> String {
    input
        .get("url")
        .and_then(serde_json::Value::as_str)
        .and_then(crate::tools::web_fetch_tool::preapproved::parse_web_fetch_url_host_path)
        .map(|(hostname, _)| format!("domain:{hostname}"))
        .unwrap_or_else(|| format!("input:{}", js_like_to_string(input)))
}

fn js_like_to_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Array(_) => "".to_string(),
        serde_json::Value::Object(_) => "[object Object]".to_string(),
    }
}

/// Maps to: CC `WebFetchPermissionRequest.tsx:38`
/// `const { url } = toolUseConfirm.input as { url: string }` — "url is already
/// validated by the input schema" (`:37`), so CC reads the field and nothing
/// else.
///
/// The removed `request.input_summary` fallback was actively wrong here: for
/// WebFetch that field is `domain:{hostname}` / `input:{…}`
/// (`permissions.rs#web_fetch_permission_rule_content`, CC
/// `webFetchToolInputToPermissionRuleContent`), i.e. a rule string, never a URL.
pub fn web_fetch_url_from_request(request: &PermissionRequestData) -> Option<String> {
    request
        .input
        .get("url")
        .and_then(serde_json::Value::as_str)
        .filter(|url| !url.trim().is_empty())
        .map(str::to_string)
}

/// Maps to: CC `new URL(url).hostname`.
pub fn web_fetch_hostname_from_url(url: &str) -> Option<String> {
    crate::tools::web_fetch_tool::preapproved::parse_web_fetch_url_host_path(url)
        .map(|(hostname, _)| hostname)
}

/// Maps to: CC `WebFetchTool.renderToolUseMessage(...)` for permission preview.
pub fn web_fetch_permission_preview(request: &PermissionRequestData, verbose: bool) -> String {
    let Some(url) = web_fetch_url_from_request(request) else {
        return String::new();
    };
    let prompt = request
        .input
        .get("prompt")
        .and_then(serde_json::Value::as_str);
    crate::tools::web_fetch_tool::ui::render_tool_use_message(Some(&url), prompt, verbose)
        .unwrap_or_default()
}

/// Maps to: CC WebFetch domain-specific `options` construction.
pub fn web_fetch_permission_options(
    hostname: &str,
    show_always_allow_options: bool,
) -> Vec<WebFetchPermissionOption> {
    let mut options = vec![WebFetchPermissionOption::select(
        "Yes",
        WebFetchPermissionOptionValue::Yes,
    )];
    if show_always_allow_options {
        options.push(WebFetchPermissionOption::select(
            format!("Yes, and don't ask again for {hostname}"),
            WebFetchPermissionOptionValue::YesDontAskAgainDomain,
        ));
    }
    options.push(WebFetchPermissionOption::select(
        "No, and tell Claude what to do differently (esc)",
        WebFetchPermissionOptionValue::No,
    ));
    options
}

/// Maps to: CC `onChange(...)` branches.
pub fn web_fetch_permission_option_to_prompt_choice(
    value: WebFetchPermissionOptionValue,
) -> PermissionPromptChoice {
    match value {
        WebFetchPermissionOptionValue::Yes => PermissionPromptChoice::AllowOnce,
        WebFetchPermissionOptionValue::YesDontAskAgainDomain => PermissionPromptChoice::AlwaysAllow,
        WebFetchPermissionOptionValue::No => PermissionPromptChoice::Deny,
    }
}

/// Maps to CC `WebFetchPermissionRequest.tsx#onChange` domain allow update.
pub fn web_fetch_permission_option_to_prompt_response(
    value: WebFetchPermissionOptionValue,
    request: &PermissionRequestData,
) -> PermissionPromptResponse {
    let choice = web_fetch_permission_option_to_prompt_choice(value);
    let mut response = PermissionPromptResponse::new(choice);
    if value == WebFetchPermissionOptionValue::YesDontAskAgainDomain {
        response = response.with_permission_updates(vec![PermissionUpdate::AddRules {
            destination: PermissionUpdateDestination::LocalSettings,
            behavior: PermissionBehavior::Allow,
            rules: vec![crate::types::permissions::PermissionRuleValue::new(
                request.tool_name.clone(),
                Some(web_fetch_input_to_permission_rule_content(&request.input)),
            )],
        }]);
    }
    response
}

/// Maps to: CC `WebFetchPermissionRequest` render path.
#[component]
pub fn WebFetchPermissionRequest<'a>(
    props: &mut WebFetchPermissionRequestProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let request = props.request.clone().unwrap_or_else(default_request);
    // Re-derived 2026-08-26 (#134): CC `WebFetchPermissionRequest.tsx:120-129`
    // passes the component's `verbose` PROP, not a literal `true`. The port
    // hardcoded `true` only because no producer existed above this component.
    let preview = web_fetch_permission_preview(&request, props.verbose);
    let url = web_fetch_url_from_request(&request).unwrap_or_default();
    let hostname = web_fetch_hostname_from_url(&url).unwrap_or_else(|| url.clone());
    let options = web_fetch_permission_options(&hostname, props.show_always_allow_options);
    let option_count = options.len().max(1);
    let mut focused_index = hooks.use_state(|| 0usize);
    let mut pending_select = hooks.use_state(|| Option::<WebFetchPermissionOptionValue>::None);

    hooks.use_terminal_events({
        let mut focused_index = focused_index;
        let mut pending_select = pending_select;
        let options = options.clone();
        move |event| {
            let TerminalEvent::Key(KeyEvent {
                code,
                kind,
                ..
            }) = event
            else {
                return;
            };
            if kind == KeyEventKind::Release {
                return;
            }
            match code {
                KeyCode::Up | KeyCode::Char('k') => {
                    focused_index.set(focused_index.get().saturating_sub(1));
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                    focused_index
                        .set((focused_index.get() + 1).min(options.len().saturating_sub(1)));
                }
                KeyCode::Enter => {
                    if let Some(option) = options.get(focused_index.get()) {
                        pending_select.set(Some(option.value));
                    }
                }
                KeyCode::Esc => {
                    // CC PermissionPrompt.onCancel in WebFetchPermissionRequest
                    // rejects the fetch rather than emitting a generic cancel.
                    pending_select.set(Some(WebFetchPermissionOptionValue::No));
                }
                // No Ctrl+C here: CC's is PermissionRequest's app:interrupt,
                // and Cometix binds Ctrl+C to app:exit (2.0.x semantics).
                _ => {}
            }
        }
    });

    let selected = { pending_select.read().clone() };
    if let Some(value) = selected {
        pending_select.set(None);
        (props.on_select)(value);
    }

    let focused = focused_index.get().min(option_count - 1);
    let select_options = options
        .iter()
        .map(WebFetchPermissionOption::to_select_option)
        .collect::<Vec<_>>();

    element! {
        PermissionDialog(
            title: "Fetch".to_string(),
            worker_badge: props.worker_badge.clone(),
        ) {
            View(flex_direction: FlexDirection::Column, padding_left: 2u32, padding_right: 2u32, padding_top: 1u32, padding_bottom: 1u32) {
                #(if preview.is_empty() {
                    None
                } else {
                    Some(element! { Text(content: preview.clone(), wrap: TextWrap::Wrap) })
                })
                #(if request.description.trim().is_empty() {
                    None
                } else {
                    Some(element! { Text(content: request.description.clone(), color: theme.inactive, wrap: TextWrap::Wrap) })
                })
            }
            View(flex_direction: FlexDirection::Column) {
                // Maps to: CC `WebFetchPermissionRequest.tsx:135-138` — mounted
                // inside the same `flexDirection="column"` Box, immediately
                // before the "Do you want to allow Claude to fetch this
                // content?" line.
                PermissionRuleExplanation(
                    decision_reason: request.decision_reason.clone(),
                    tool_type: PermissionRuleToolType::Tool,
                    permission_mode: request.mode,
                )
                Text(content: "Do you want to allow Claude to fetch this content?".to_string(), wrap: TextWrap::NoWrap)
                Select(
                    options: select_options,
                    focused_index: focused,
                    visible_option_count: option_count,
                    layout: SelectLayout::Compact,
                    hide_indexes: true,
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::permissions::{PermissionMode, PermissionRuleValue};
    use crate::utils::theme;
    use futures::{StreamExt, stream};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn key(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    fn web_fetch_request() -> PermissionRequestData {
        PermissionRequestData {
            permission_result: None,
            id: "req".to_string(),
            tool_use_id: "toolu".to_string(),
            tool_name: "WebFetch".to_string(),
            mcp_info: None,
            decision_reason: None,
            description: "Fetch https://Example.com/docs".to_string(),
            message: String::new(),
            input_summary: "https://Example.com/docs".to_string(),
            input: serde_json::json!({
                "url": "https://Example.com/docs",
                "prompt": "summarize"
            }),
            call_input: None,
            rule: PermissionRuleValue::new("WebFetch", Some("domain:example.com".to_string())),
            suggestions: Vec::new(),
            blocked_path: None,
            metadata: None,
            is_compound_command: false,
            mode: PermissionMode::Default,
        }
    }

    fn render_web_fetch(props: WebFetchPermissionRequestProps<'static>) -> String {
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                WebFetchPermissionRequest(
                    request: props.request,
                    worker_badge: props.worker_badge,
                    show_always_allow_options: props.show_always_allow_options,
                )
            }
        }
        .render(Some(120))
        .to_string()
    }

    #[test]
    fn web_fetch_permission_helpers_match_official_domain_rule_and_options() {
        let request = web_fetch_request();
        assert_eq!(
            web_fetch_input_to_permission_rule_content(&request.input),
            "domain:example.com"
        );
        assert_eq!(
            web_fetch_input_to_permission_rule_content(
                &serde_json::json!({ "url": "mailto:dev@example.com" })
            ),
            "input:[object Object]"
        );
        assert_eq!(
            web_fetch_hostname_from_url("https://User:pass@Example.COM:443/path").as_deref(),
            Some("example.com")
        );
        // `WebFetchTool/UI.tsx:14-20`: bare url when not verbose, the
        // `url: "…", prompt: "…"` form when verbose.
        assert_eq!(
            web_fetch_permission_preview(&request, false),
            "https://Example.com/docs"
        );
        assert_eq!(
            web_fetch_permission_preview(&request, true),
            "url: \"https://Example.com/docs\", prompt: \"summarize\""
        );

        let options = web_fetch_permission_options("example.com", true);
        assert_eq!(
            options
                .iter()
                .map(|option| option.value)
                .collect::<Vec<_>>(),
            vec![
                WebFetchPermissionOptionValue::Yes,
                WebFetchPermissionOptionValue::YesDontAskAgainDomain,
                WebFetchPermissionOptionValue::No,
            ]
        );
        assert_eq!(options[1].label, "Yes, and don't ask again for example.com");
        assert_eq!(
            options[2].label,
            "No, and tell Claude what to do differently (esc)"
        );
        assert_eq!(
            web_fetch_permission_option_to_prompt_choice(
                WebFetchPermissionOptionValue::YesDontAskAgainDomain
            ),
            PermissionPromptChoice::AlwaysAllow
        );
    }

    #[test]
    fn web_fetch_permission_request_renders_official_dialog_copy() {
        let text = render_web_fetch(WebFetchPermissionRequestProps {
            request: Some(web_fetch_request()),
            worker_badge: Some(WorkerBadgeProps {
                name: "reader".to_string(),
                color: None,
            }),
            show_always_allow_options: true,
            ..WebFetchPermissionRequestProps::default()
        });

        assert!(text.contains("Fetch"), "canvas=\n{text}");
        assert!(text.contains("· @reader"), "canvas=\n{text}");
        // Re-derived 2026-08-26 (#134): `WebFetchPermissionRequest.tsx:120-129`
        // passes the component's `verbose` PROP into
        // `WebFetchTool.renderToolUseMessage`, and `WebFetchTool/UI.tsx:14-20`
        // returns the bare url unless verbose. This fixture leaves `verbose` at
        // its default (false), so the bare url is the official rendering; the
        // `url: "…", prompt: "…"` form asserted before was transcribing the
        // port's hardcoded `verbose: true`.
        assert!(
            !text.contains("url: \"https://Example.com/docs\""),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Fetch https://Example.com/docs"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Do you want to allow Claude to fetch this content?"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Yes"), "canvas=\n{text}");
        assert!(
            text.contains("Yes, and don't ask again for example.com"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("No, and tell Claude what to do differently (esc)"),
            "canvas=\n{text}"
        );
        assert!(
            !text.contains("Do you want to proceed?"),
            "WebFetch uses official domain-specific prompt copy, not generic PermissionPrompt; canvas=\n{text}"
        );
    }

    /// Maps to: CC `WebFetchPermissionRequest.tsx:134-139` —
    /// `<PermissionRuleExplanation permissionResult={toolUseConfirm.permissionResult}
    /// toolType="tool" />` is the first child of the `flexDirection="column"`
    /// Box, immediately before "Do you want to allow Claude to fetch this
    /// content?". `toolType` is `"tool"` at `:137`, and
    /// `PermissionRuleExplanation.tsx:95-97` renders nothing without a reason.
    #[test]
    fn web_fetch_permission_request_matches_official_rule_explanation_mount() {
        let mut request = web_fetch_request();
        request.decision_reason = Some(
            crate::utils::permissions::permission_result::PermissionDecisionReason::Rule {
                rule: crate::types::permissions::PermissionRule {
                    source: crate::types::permissions::PermissionRuleSource::LocalSettings,
                    rule_behavior: PermissionBehavior::Ask,
                    rule_value: PermissionRuleValue::new(
                        "WebFetch",
                        Some("domain:example.com".to_string()),
                    ),
                },
            },
        );

        let with_reason = render_web_fetch(WebFetchPermissionRequestProps {
            request: Some(request),
            show_always_allow_options: true,
            ..WebFetchPermissionRequestProps::default()
        });

        assert!(
            with_reason.contains(
                "Permission rule WebFetch(domain:example.com) requires confirmation for this"
            ),
            "canvas=\n{with_reason}"
        );
        assert!(
            with_reason.contains("tool."),
            "toolType is \"tool\" at CC :137; canvas=\n{with_reason}"
        );
        assert!(
            with_reason.contains("/permissions to update rules"),
            "canvas=\n{with_reason}"
        );
        // CC :135-138 sits ABOVE the :139 prompt line.
        let explanation_at = with_reason
            .find("Permission rule WebFetch(domain:example.com)")
            .expect("explanation rendered");
        let prompt_at = with_reason
            .find("Do you want to allow Claude to fetch this content?")
            .expect("prompt rendered");
        assert!(
            explanation_at < prompt_at,
            "explanation must precede the fetch prompt; canvas=\n{with_reason}"
        );

        let without_reason = render_web_fetch(WebFetchPermissionRequestProps {
            request: Some(web_fetch_request()),
            show_always_allow_options: true,
            ..WebFetchPermissionRequestProps::default()
        });
        assert!(
            !without_reason.contains("Permission rule"),
            "canvas=\n{without_reason}"
        );
        assert!(
            !without_reason.contains("/permissions to update rules"),
            "canvas=\n{without_reason}"
        );
    }

    #[test]
    fn web_fetch_permission_request_enter_and_escape_dispatch_official_option_values() {
        let selected = Arc::new(Mutex::new(Vec::new()));
        let selected_for_handler = Arc::clone(&selected);

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(*theme::current())) {
                    WebFetchPermissionRequest(
                        request: Some(web_fetch_request()),
                        show_always_allow_options: true,
                        on_select: move |value| {
                            selected_for_handler.lock().expect("selected mutex").push(value);
                        },
                    )
                }
            };
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(stream::iter(vec![
                        key(KeyCode::Down),
                        key(KeyCode::Enter),
                        key(KeyCode::Esc),
                    ]))
                    .with_size(120, 30),
                ),
            );
            for _ in 0..10 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                if next.is_none() {
                    break;
                }
            }
        });

        assert_eq!(
            selected.lock().expect("selected mutex").as_slice(),
            &[WebFetchPermissionOptionValue::No]
        );
    }
}
