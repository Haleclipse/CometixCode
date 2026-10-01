//! Maps to: CC `components/Feedback.tsx`.
//!
//! This file ports the feedback dialog state rendering and pure data helpers.
//! Telemetry is intentionally not implemented. The network/OAuth submission path
//! is represented by an explicit safe-disabled seam because this repository's
//! standing safety policy forbids executing OAuth/apiKeyHelper/external login
//! flows outside a dedicated auth/network slice.

use iocraft::prelude::*;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

pub const GITHUB_URL_LIMIT: usize = 7250;
pub const GITHUB_ISSUES_REPO_URL: &str = "https://github.com/anthropics/claude-code/issues";

static QUOTED_ANTHROPIC_KEY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#""(sk-ant[^\s"']{24,})""#).expect("valid anthropic quoted key regex")
});
static ANTHROPIC_KEY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(^|[^A-Za-z0-9"'])(sk-ant-?[A-Za-z0-9_-]{10,})([^A-Za-z0-9"']|$)"#)
        .expect("valid anthropic key regex")
});
static AWS_LABEL_KEY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"AWS key: "(AWS[A-Z0-9]{20,})""#).expect("valid aws label regex")
});
static AWS_AKIA_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(AKIA[A-Z0-9]{16})").expect("valid aws akia regex"));
static GCP_KEY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(^|[^A-Za-z0-9])(AIza[A-Za-z0-9_-]{35})([^A-Za-z0-9]|$)")
        .expect("valid gcp key regex")
});
static GCP_SERVICE_ACCOUNT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(^|[^A-Za-z0-9])([a-z0-9-]+@[a-z0-9-]+\.iam\.gserviceaccount\.com)([^A-Za-z0-9]|$)",
    )
    .expect("valid gcp service account regex")
});
static X_API_KEY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(["']?x-api-key["']?\s*[:=]\s*["']?)[^"',\s\)}\]]+"#)
        .expect("valid x-api-key regex")
});
static AUTHORIZATION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(["']?authorization["']?\s*[:=]\s*["']?(bearer\s+)?)[^"',\s\)}\]]+"#)
        .expect("valid authorization regex")
});
static AWS_ENV_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(AWS[_-][A-Za-z0-9_]+\s*[=:]\s*)["']?[^"',\s\)}\]]+["']?"#)
        .expect("valid aws env regex")
});
static GCP_ENV_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(GOOGLE[_-][A-Za-z0-9_]+\s*[=:]\s*)["']?[^"',\s\)}\]]+["']?"#)
        .expect("valid gcp env regex")
});
static GENERIC_SECRET_ENV_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)((API[-_]?KEY|TOKEN|SECRET|PASSWORD)\s*[=:]\s*)["']?[^"',\s\)}\]]+["']?"#)
        .expect("valid generic secret env regex")
});

/// Maps to: CC `Feedback.tsx#redactSensitiveInfo`.
pub fn redact_sensitive_info(text: &str) -> String {
    let mut redacted = text.to_string();
    redacted = QUOTED_ANTHROPIC_KEY_RE
        .replace_all(&redacted, r#""[REDACTED_API_KEY]""#)
        .into_owned();
    redacted = ANTHROPIC_KEY_RE
        .replace_all(&redacted, "${1}[REDACTED_API_KEY]${3}")
        .into_owned();
    redacted = AWS_LABEL_KEY_RE
        .replace_all(&redacted, r#"AWS key: "[REDACTED_AWS_KEY]""#)
        .into_owned();
    redacted = AWS_AKIA_RE
        .replace_all(&redacted, "[REDACTED_AWS_KEY]")
        .into_owned();
    redacted = GCP_KEY_RE
        .replace_all(&redacted, "${1}[REDACTED_GCP_KEY]${3}")
        .into_owned();
    redacted = GCP_SERVICE_ACCOUNT_RE
        .replace_all(&redacted, "${1}[REDACTED_GCP_SERVICE_ACCOUNT]${3}")
        .into_owned();
    redacted = X_API_KEY_RE
        .replace_all(&redacted, "${1}[REDACTED_API_KEY]")
        .into_owned();
    redacted = AUTHORIZATION_RE
        .replace_all(&redacted, "${1}[REDACTED_TOKEN]")
        .into_owned();
    redacted = AWS_ENV_RE
        .replace_all(&redacted, "${1}[REDACTED_AWS_VALUE]")
        .into_owned();
    redacted = GCP_ENV_RE
        .replace_all(&redacted, "${1}[REDACTED_GCP_VALUE]")
        .into_owned();
    GENERIC_SECRET_ENV_RE
        .replace_all(&redacted, "${1}[REDACTED]")
        .into_owned()
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedbackErrorLog {
    pub error: Option<String>,
    pub timestamp: Option<String>,
}

/// Maps to: CC `Feedback.tsx#getSanitizedErrorLogs` redaction transform.
pub fn sanitize_error_logs(errors: &[FeedbackErrorLog]) -> Vec<FeedbackErrorLog> {
    errors
        .iter()
        .map(|error| FeedbackErrorLog {
            error: error.error.as_deref().map(redact_sensitive_info),
            timestamp: error.timestamp.clone(),
        })
        .collect()
}

/// Maps to: JS `encodeURIComponent` used by CC `createGitHubIssueUrl`.
pub fn encode_uri_component(input: &str) -> String {
    let mut encoded = String::new();
    for byte in input.as_bytes() {
        match *byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => encoded.push(*byte as char),
            byte => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn trim_encoded_percent_sequence(encoded: &mut String) {
    if let Some(last_percent) = encoded.rfind('%') {
        if last_percent >= encoded.len().saturating_sub(2) {
            encoded.truncate(last_percent);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedbackIssueEnvironment {
    pub platform: String,
    pub terminal: String,
    pub version: Option<String>,
}

impl Default for FeedbackIssueEnvironment {
    fn default() -> Self {
        Self {
            platform: std::env::consts::OS.to_string(),
            terminal: crate::utils::process_env::var("TERM").unwrap_or_else(|| "unknown".to_string()),
            version: option_env!("CARGO_PKG_VERSION").map(ToOwned::to_owned),
        }
    }
}

/// Maps to: CC `Feedback.tsx#createGitHubIssueUrl`.
pub fn create_github_issue_url(
    feedback_id: &str,
    title: &str,
    description: &str,
    errors: &[FeedbackErrorLog],
) -> String {
    create_github_issue_url_with_env(
        feedback_id,
        title,
        description,
        errors,
        &FeedbackIssueEnvironment::default(),
    )
}

/// Maps to: CC `Feedback.tsx#createGitHubIssueUrl`, with env injected for tests.
pub fn create_github_issue_url_with_env(
    feedback_id: &str,
    title: &str,
    description: &str,
    errors: &[FeedbackErrorLog],
    env: &FeedbackIssueEnvironment,
) -> String {
    let sanitized_title = redact_sensitive_info(title);
    let sanitized_description = redact_sensitive_info(description);
    let version = env.version.as_deref().unwrap_or("unknown");
    let body_prefix = format!(
        "**Bug Description**\n{sanitized_description}\n\n**Environment Info**\n- Platform: {}\n- Terminal: {}\n- Version: {}\n- Feedback ID: {feedback_id}\n\n**Errors**\n```json\n",
        env.platform, env.terminal, version
    );
    let error_suffix = "\n```\n";
    let errors_json = serde_json::to_string(errors).unwrap_or_else(|_| "[]".to_string());
    let base_url = format!(
        "{GITHUB_ISSUES_REPO_URL}/new?title={}&labels=user-reported,bug&body=",
        encode_uri_component(&sanitized_title)
    );
    let truncation_note = "\n**Note:** Content was truncated.\n";

    let encoded_prefix = encode_uri_component(&body_prefix);
    let encoded_suffix = encode_uri_component(error_suffix);
    let encoded_note = encode_uri_component(truncation_note);
    let encoded_errors = encode_uri_component(&errors_json);
    let space_for_errors = GITHUB_URL_LIMIT as isize
        - base_url.len() as isize
        - encoded_prefix.len() as isize
        - encoded_suffix.len() as isize
        - encoded_note.len() as isize;

    if space_for_errors <= 0 {
        let ellipsis = encode_uri_component("…");
        let buffer = 50usize;
        let max_encoded_length = GITHUB_URL_LIMIT
            .saturating_sub(base_url.len())
            .saturating_sub(ellipsis.len())
            .saturating_sub(encoded_note.len())
            .saturating_sub(buffer);
        let full_body = format!("{body_prefix}{errors_json}{error_suffix}");
        let mut encoded_full_body = encode_uri_component(&full_body);
        if encoded_full_body.len() > max_encoded_length {
            encoded_full_body.truncate(max_encoded_length);
            trim_encoded_percent_sequence(&mut encoded_full_body);
        }
        return format!("{base_url}{encoded_full_body}{ellipsis}{encoded_note}");
    }

    if encoded_errors.len() <= space_for_errors as usize {
        return format!("{base_url}{encoded_prefix}{encoded_errors}{encoded_suffix}");
    }

    let ellipsis = encode_uri_component("…");
    let buffer = 50usize;
    let mut truncated_encoded_errors = encoded_errors
        .chars()
        .take(
            (space_for_errors as usize)
                .saturating_sub(ellipsis.len())
                .saturating_sub(buffer),
        )
        .collect::<String>();
    trim_encoded_percent_sequence(&mut truncated_encoded_errors);
    format!(
        "{base_url}{encoded_prefix}{truncated_encoded_errors}{ellipsis}{encoded_suffix}{encoded_note}"
    )
}

/// Maps to: CC `Feedback.tsx#createFallbackTitle`.
pub fn create_fallback_title(description: &str) -> String {
    let first_line = description.split('\n').next().unwrap_or("");
    if first_line.len() <= 60 && first_line.len() > 5 {
        return first_line.to_string();
    }

    let mut truncated = first_line.chars().take(60).collect::<String>();
    if first_line.chars().count() > 60 {
        if let Some(last_space) = truncated.rfind(' ') {
            if last_space > 30 {
                truncated.truncate(last_space);
            }
        }
        truncated.push_str("...");
    }

    if truncated.len() < 10 {
        "Bug Report".to_string()
    } else {
        truncated
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedbackData {
    pub latest_assistant_message_id: Option<String>,
    pub message_count: usize,
    pub datetime: String,
    pub description: String,
    pub platform: String,
    pub git_repo: bool,
    pub version: Option<String>,
    pub raw_transcript_jsonl: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeedbackSubmitResult {
    pub success: bool,
    pub feedback_id: Option<String>,
    pub is_zdr_org: bool,
    pub disabled_by_policy: bool,
}

/// Maps to: CC `Feedback.tsx#submitFeedback`.
///
/// Current safe behavior: returns `disabled_by_policy=true` and performs no
/// OAuth refresh, auth-header lookup, or network POST. Follow-up parity work
/// should wire this to `services/api`/auth once the active slice explicitly
/// permits external feedback submission.
pub async fn submit_feedback_safe_disabled(
    _data: FeedbackData,
    _signal_cancelled: bool,
) -> FeedbackSubmitResult {
    FeedbackSubmitResult {
        success: false,
        disabled_by_policy: true,
        ..FeedbackSubmitResult::default()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FeedbackStep {
    #[default]
    UserInput,
    Consent,
    Submitting,
    Done,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeedbackGitState {
    pub branch_name: String,
    pub commit_hash: Option<String>,
    pub remote_url: Option<String>,
    pub is_head_on_remote: bool,
    pub is_clean: bool,
}

/// Maps to: CC `Feedback.tsx` consent git metadata string.
pub fn feedback_git_state_summary(git_state: &FeedbackGitState) -> String {
    let mut summary = git_state.branch_name.clone();
    if let Some(commit_hash) = git_state.commit_hash.as_deref() {
        summary.push_str(", ");
        summary.push_str(&commit_hash.chars().take(7).collect::<String>());
    }
    if let Some(remote_url) = git_state.remote_url.as_deref() {
        summary.push_str(" @ ");
        summary.push_str(remote_url);
    }
    if !git_state.is_head_on_remote {
        summary.push_str(", not synced");
    }
    if !git_state.is_clean {
        summary.push_str(", has local changes");
    }
    summary
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedbackViewData {
    pub step: FeedbackStep,
    pub description: String,
    pub error: Option<String>,
    pub feedback_id: Option<String>,
    pub title: Option<String>,
    pub platform: String,
    pub terminal: String,
    pub version: Option<String>,
    pub git_state: Option<FeedbackGitState>,
    pub exit_pending: bool,
    pub exit_key_name: String,
}

impl Default for FeedbackViewData {
    fn default() -> Self {
        let env = FeedbackIssueEnvironment::default();
        Self {
            step: FeedbackStep::UserInput,
            description: String::new(),
            error: None,
            feedback_id: None,
            title: None,
            platform: env.platform,
            terminal: env.terminal,
            version: env.version,
            git_state: None,
            exit_pending: false,
            exit_key_name: "Esc".to_string(),
        }
    }
}

/// Maps to: CC `Feedback.tsx` `Dialog.inputGuide` branches.
pub fn feedback_input_guide(data: &FeedbackViewData) -> Option<String> {
    if data.exit_pending {
        return Some(format!("Press {} again to exit", data.exit_key_name));
    }
    match data.step {
        FeedbackStep::UserInput => Some("Enter to continue · Esc to cancel".to_string()),
        FeedbackStep::Consent => Some("Enter to submit · Esc to cancel".to_string()),
        FeedbackStep::Submitting | FeedbackStep::Done => None,
    }
}

#[derive(Default, Props)]
pub struct FeedbackProps {
    pub data: FeedbackViewData,
}

/// Maps to: CC `components/Feedback.tsx#Feedback` render states.
#[component]
pub fn Feedback(props: &FeedbackProps) -> impl Into<AnyElement<'static>> {
    let data = props.data.clone();
    let guide = feedback_input_guide(&data);
    let version = data.version.as_deref().unwrap_or("unknown").to_string();

    element! {
        View(flex_direction: FlexDirection::Column, border_style: BorderStyle::Round, border_color: Color::Cyan, padding_left: 1u32, padding_right: 1u32) {
            Text(content: "Submit Feedback / Bug Report", weight: Weight::Bold, color: Color::Cyan, wrap: TextWrap::NoWrap)
            #(match data.step {
                FeedbackStep::UserInput => element! {
                    View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                        Text(content: "Describe the issue below:")
                        Text(content: data.description.clone(), wrap: TextWrap::Wrap)
                        #(data.error.as_ref().map(|error| element! {
                            View(flex_direction: FlexDirection::Column) {
                                Text(content: error.clone(), color: Color::Red, wrap: TextWrap::Wrap)
                                Text(content: "Edit and press Enter to retry, or Esc to cancel", dim: true, wrap: TextWrap::NoWrap)
                            }
                        }))
                    }
                }.into_any(),
                FeedbackStep::Consent => element! {
                    View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                        Text(content: "This report will include:")
                        View(flex_direction: FlexDirection::Column, margin_left: 2u32) {
                            Text(content: format!("- Your feedback / bug description: {}", data.description), wrap: TextWrap::Wrap)
                            Text(content: format!("- Environment info: {}, {}, v{}", data.platform, data.terminal, version), wrap: TextWrap::NoWrap)
                            #(data.git_state.as_ref().map(|git_state| element! {
                                Text(content: format!("- Git repo metadata: {}", feedback_git_state_summary(git_state)), wrap: TextWrap::Wrap)
                            }))
                            Text(content: "- Current session transcript")
                        }
                        Text(content: "We will use your feedback to debug related issues or to improve Claude Code's functionality (eg. to reduce the risk of bugs occurring in the future).", dim: true, wrap: TextWrap::Wrap)
                        Text(content: "Press Enter to confirm and submit.")
                    }
                }.into_any(),
                FeedbackStep::Submitting => element! {
                    View(flex_direction: FlexDirection::Row, margin_top: 1u32) {
                        Text(content: "Submitting report…")
                    }
                }.into_any(),
                FeedbackStep::Done => element! {
                    View(flex_direction: FlexDirection::Column, margin_top: 1u32) {
                        #(if let Some(error) = data.error.as_ref() {
                            element! { Text(content: error.clone(), color: Color::Red, wrap: TextWrap::Wrap) }
                        } else {
                            element! { Text(content: "Thank you for your report!", color: Color::Green, wrap: TextWrap::NoWrap) }
                        })
                        #(data.feedback_id.as_ref().map(|feedback_id| element! {
                            Text(content: format!("Feedback ID: {feedback_id}"), dim: true, wrap: TextWrap::NoWrap)
                        }))
                        Text(content: "Press Enter to open your browser and draft a GitHub issue, or any other key to close.", wrap: TextWrap::Wrap)
                    }
                }.into_any(),
            })
            #(guide.map(|guide| element! {
                Text(content: guide, dim: true, italic: true, wrap: TextWrap::NoWrap)
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feedback_redacts_sensitive_values_like_official() {
        let gcp_key = format!("AIza{}", "A".repeat(35));
        let input = format!(
            "api sk-ant-api03_abcdefghijklmnop x-api-key: secret authorization: Bearer token AWS_SECRET_ACCESS_KEY=abc GOOGLE_TOKEN=def AKIAABCDEFGHIJKLMNOP {gcp_key} service@test.iam.gserviceaccount.com password=hunter2"
        );
        let redacted = redact_sensitive_info(&input);
        assert!(!redacted.contains("sk-ant-api03"), "{redacted}");
        assert!(!redacted.contains("secret"), "{redacted}");
        assert!(!redacted.contains("Bearer token"), "{redacted}");
        assert!(!redacted.contains("AKIAABCDEFGHIJKLMNOP"), "{redacted}");
        assert!(!redacted.contains(&gcp_key), "{redacted}");
        assert!(
            !redacted.contains("service@test.iam.gserviceaccount.com"),
            "{redacted}"
        );
        assert!(!redacted.contains("hunter2"), "{redacted}");
        assert!(redacted.contains("[REDACTED_API_KEY]"), "{redacted}");
        assert!(redacted.contains("[REDACTED_TOKEN]"), "{redacted}");
    }

    #[test]
    fn feedback_fallback_title_matches_official_word_boundary_rules() {
        assert_eq!(create_fallback_title("short"), "Bug Report");
        assert_eq!(
            create_fallback_title("A useful bug title"),
            "A useful bug title"
        );
        assert_eq!(
            create_fallback_title(
                "This is a very long bug report description that should be truncated at a word boundary when possible"
            ),
            "This is a very long bug report description that should be..."
        );
    }

    #[test]
    fn feedback_github_issue_url_redacts_and_truncates() {
        let env = FeedbackIssueEnvironment {
            platform: "darwin".to_string(),
            terminal: "xterm-kitty".to_string(),
            version: Some("1.2.3".to_string()),
        };
        let errors = vec![FeedbackErrorLog {
            error: Some("authorization: Bearer token".to_string()),
            timestamp: Some("now".to_string()),
        }];
        let url = create_github_issue_url_with_env(
            "fb_123",
            "[Bug] sk-ant-api03_abcdefghijklmnop leaked",
            "description with x-api-key: secret",
            &sanitize_error_logs(&errors),
            &env,
        );
        assert!(url.starts_with("https://github.com/anthropics/claude-code/issues/new?"));
        assert!(url.contains("labels=user-reported,bug"));
        assert!(url.contains("fb_123"));
        assert!(!url.contains("sk-ant"), "{url}");
        assert!(!url.contains("secret"), "{url}");
        assert!(!url.contains("token"), "{url}");

        let long = "x".repeat(10_000);
        let truncated = create_github_issue_url_with_env("fb", "title", &long, &[], &env);
        assert!(
            truncated.len() <= GITHUB_URL_LIMIT + 20,
            "len={}",
            truncated.len()
        );
        assert!(truncated.contains("Content%20was%20truncated"));
    }

    #[test]
    fn feedback_submit_is_explicitly_safe_disabled() {
        let result = futures::executor::block_on(submit_feedback_safe_disabled(
            FeedbackData::default(),
            false,
        ));
        assert!(!result.success);
        assert!(result.disabled_by_policy);
    }

    #[test]
    fn feedback_git_state_summary_matches_official_order() {
        assert_eq!(
            feedback_git_state_summary(&FeedbackGitState {
                branch_name: "main".to_string(),
                commit_hash: Some("abcdef123456".to_string()),
                remote_url: Some("git@example.com/repo".to_string()),
                is_head_on_remote: false,
                is_clean: false,
            }),
            "main, abcdef1 @ git@example.com/repo, not synced, has local changes"
        );
    }

    #[test]
    fn feedback_renders_user_input_consent_submitting_and_done_states() {
        let input = element! {
            Feedback(data: FeedbackViewData {
                description: "It broke".to_string(),
                ..FeedbackViewData::default()
            })
        }
        .render(Some(100))
        .to_string();
        assert!(
            input.contains("Submit Feedback / Bug Report"),
            "canvas=\n{input}"
        );
        assert!(
            input.contains("Describe the issue below"),
            "canvas=\n{input}"
        );
        assert!(input.contains("Enter to continue"), "canvas=\n{input}");

        let consent = element! {
            Feedback(data: FeedbackViewData {
                step: FeedbackStep::Consent,
                description: "It broke".to_string(),
                platform: "darwin".to_string(),
                terminal: "kitty".to_string(),
                version: Some("1.2.3".to_string()),
                git_state: Some(FeedbackGitState {
                    branch_name: "main".to_string(),
                    commit_hash: Some("abcdef123456".to_string()),
                    remote_url: None,
                    is_head_on_remote: true,
                    is_clean: true,
                }),
                ..FeedbackViewData::default()
            })
        }
        .render(Some(120))
        .to_string();
        assert!(
            consent.contains("This report will include"),
            "canvas=\n{consent}"
        );
        assert!(
            consent.contains("darwin, kitty, v1.2.3"),
            "canvas=\n{consent}"
        );
        assert!(consent.contains("main, abcdef1"), "canvas=\n{consent}");
        assert!(consent.contains("Enter to submit"), "canvas=\n{consent}");

        let submitting = element! {
            Feedback(data: FeedbackViewData { step: FeedbackStep::Submitting, ..FeedbackViewData::default() })
        }
        .render(Some(80))
        .to_string();
        assert!(
            submitting.contains("Submitting report…"),
            "canvas=\n{submitting}"
        );

        let done = element! {
            Feedback(data: FeedbackViewData {
                step: FeedbackStep::Done,
                feedback_id: Some("fb_123".to_string()),
                ..FeedbackViewData::default()
            })
        }
        .render(Some(100))
        .to_string();
        assert!(
            done.contains("Thank you for your report"),
            "canvas=\n{done}"
        );
        assert!(done.contains("Feedback ID: fb_123"), "canvas=\n{done}");
        assert!(done.contains("draft a GitHub issue"), "canvas=\n{done}");
    }
}
