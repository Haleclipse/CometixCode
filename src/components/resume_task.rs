//! Maps to: CC `components/ResumeTask.tsx`.
//!
//! The official component owns API loading (`fetchCodeSessionsFromSessionsAPI`),
//! repository detection, keybindings, and retry side effects. Cometix keeps
//! those side effects outside this render slice for now; callers provide a
//! snapshot state while this module preserves official data shaping, error
//! classification, option copy, and layout calculations.

use crate::components::configurable_shortcut_hint::ConfigurableShortcutHint;
use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::components::design_system::byline::Byline;
use crate::components::design_system::keyboard_shortcut_hint::KeyboardShortcutHint;
use crate::components::spinner::Spinner;
use crate::components::teleport_error::{TeleportError, TeleportLocalErrorType};
use crate::components::teleport_stash::TeleportStashState;
use crate::utils::format::format_relative_time_ago_millis;
use iocraft::prelude::*;

pub const RESUME_TASK_UPDATED_STRING: &str = "Updated";
pub const RESUME_TASK_SPACE_BETWEEN_TABLE_COLUMNS: &str = "  ";
const RESUME_TASK_LAYOUT_OVERHEAD: i32 = 7;

/// Maps to: CC `utils/teleport/api.ts#CodeSession` fields consumed by
/// `components/ResumeTask.tsx`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodeSession {
    pub id: String,
    pub title: String,
    pub updated_at: String,
    pub repo: Option<CodeSessionRepo>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodeSessionRepo {
    pub name: String,
    pub owner_login: String,
    pub default_branch: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeTaskLoadErrorType {
    Network,
    Auth,
    Api,
    Other,
}

impl ResumeTaskLoadErrorType {
    pub fn as_official_str(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::Auth => "auth",
            Self::Api => "api",
            Self::Other => "other",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResumeTaskState {
    TeleportError {
        current_error: Option<TeleportLocalErrorType>,
        is_logging_in: bool,
        errors_to_ignore: Vec<TeleportLocalErrorType>,
        stash_state: TeleportStashState,
    },
    Loading {
        retrying: bool,
    },
    LoadError {
        error_type: ResumeTaskLoadErrorType,
        esc_key: String,
    },
    Empty {
        current_repo: Option<String>,
        esc_key: String,
    },
    Ready {
        sessions: Vec<CodeSession>,
        current_repo: Option<String>,
        rows: usize,
        is_embedded: bool,
        focused_index: usize,
        visible_from_index: usize,
        now_ms: i64,
    },
}

impl Default for ResumeTaskState {
    fn default() -> Self {
        Self::TeleportError {
            current_error: None,
            is_logging_in: false,
            errors_to_ignore: Vec::new(),
            stash_state: TeleportStashState::Loading,
        }
    }
}

#[derive(Default, Props)]
pub struct ResumeTaskProps {
    pub state: ResumeTaskState,
}

/// Maps to: CC `ResumeTask.tsx#determineErrorType`.
pub fn determine_resume_task_error_type(error_message: &str) -> ResumeTaskLoadErrorType {
    let message = error_message.to_lowercase();
    if message.contains("fetch") || message.contains("network") || message.contains("timeout") {
        return ResumeTaskLoadErrorType::Network;
    }
    if message.contains("auth")
        || message.contains("token")
        || message.contains("permission")
        || message.contains("oauth")
        || message.contains("not authenticated")
        || message.contains("/login")
        || message.contains("console account")
        || message.contains("403")
    {
        return ResumeTaskLoadErrorType::Auth;
    }
    if message.contains("api")
        || message.contains("rate limit")
        || message.contains("500")
        || message.contains("529")
    {
        return ResumeTaskLoadErrorType::Api;
    }
    ResumeTaskLoadErrorType::Other
}

/// Maps to: CC `ResumeTask.tsx` repository filtering in `loadSessions`.
pub fn filter_sessions_for_repo(
    sessions: &[CodeSession],
    current_repo: Option<&str>,
) -> Vec<CodeSession> {
    let Some(current_repo) = current_repo else {
        return sessions.to_vec();
    };
    sessions
        .iter()
        .filter(|session| {
            session
                .repo
                .as_ref()
                .is_some_and(|repo| format!("{}/{}", repo.owner_login, repo.name) == current_repo)
        })
        .cloned()
        .collect()
}

fn parse_session_updated_at_ms(updated_at: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(updated_at)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

/// Maps to: CC `ResumeTask.tsx` newest-first sort by `updated_at`.
pub fn sort_sessions_newest_first(sessions: &[CodeSession]) -> Vec<CodeSession> {
    let mut sorted = sessions.to_vec();
    sorted.sort_by(|a, b| {
        parse_session_updated_at_ms(&b.updated_at)
            .cmp(&parse_session_updated_at_ms(&a.updated_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    sorted
}

/// Maps to: CC `formatRelativeTime(new Date(session.updated_at))` usage.
pub fn resume_task_time_string_at(updated_at: &str, now_ms: i64) -> String {
    parse_session_updated_at_ms(updated_at)
        .map(|timestamp_ms| format_relative_time_ago_millis(timestamp_ms, now_ms))
        .unwrap_or_else(|| updated_at.to_string())
}

/// Maps to: CC `ResumeTask.tsx` `maxVisibleOptions` computation.
pub fn resume_task_max_visible_options(
    session_count: usize,
    rows: usize,
    is_embedded: bool,
) -> usize {
    let rows = rows as i32;
    let available = if is_embedded {
        (rows - 6 - RESUME_TASK_LAYOUT_OVERHEAD).min(5)
    } else {
        rows - 1 - RESUME_TASK_LAYOUT_OVERHEAD
    };
    std::cmp::max(1, std::cmp::min(session_count as i32, available)) as usize
}

/// Maps to: CC `ResumeTask.tsx` session option labels.
pub fn resume_task_session_options(sessions: &[CodeSession], now_ms: i64) -> Vec<SelectOptionData> {
    let metadata = sessions
        .iter()
        .map(|session| {
            (
                session,
                resume_task_time_string_at(&session.updated_at, now_ms),
            )
        })
        .collect::<Vec<_>>();
    let max_time_width = metadata
        .iter()
        .map(|(_, time)| time.len())
        .chain(std::iter::once(RESUME_TASK_UPDATED_STRING.len()))
        .max()
        .unwrap_or(RESUME_TASK_UPDATED_STRING.len());

    metadata
        .into_iter()
        .map(|(session, time)| SelectOptionData {
            label: format!(
                "{time:<max_time_width$}{RESUME_TASK_SPACE_BETWEEN_TABLE_COLUMNS}{}",
                session.title
            ),
            value: session.id.clone(),
            ..SelectOptionData::default()
        })
        .collect()
}

/// Maps to: CC `ResumeTask.tsx#renderErrorSpecificGuidance`.
pub fn resume_task_error_guidance_lines(error_type: ResumeTaskLoadErrorType) -> Vec<&'static str> {
    match error_type {
        ResumeTaskLoadErrorType::Network => vec!["Check your internet connection"],
        ResumeTaskLoadErrorType::Auth => vec![
            "Teleport requires a Claude account",
            "Run /login and select \"Claude account with subscription\"",
        ],
        ResumeTaskLoadErrorType::Api => vec!["Sorry, Claude encountered an error"],
        ResumeTaskLoadErrorType::Other => vec!["Sorry, Claude Code encountered an error"],
    }
}

fn render_error_guidance(error_type: ResumeTaskLoadErrorType) -> AnyElement<'static> {
    let lines = resume_task_error_guidance_lines(error_type);
    element! {
        View(flex_direction: FlexDirection::Column, margin_top: 1u32, margin_bottom: 1u32) {
            #(lines.into_iter().map(|line| element! {
                Text(content: line.to_string(), dim: true, wrap: TextWrap::Wrap)
            }).collect::<Vec<_>>())
        }
    }
    .into_any()
}

/// Maps to: CC `components/ResumeTask.tsx#ResumeTask`.
#[component]
pub fn ResumeTask(props: &ResumeTaskProps) -> impl Into<AnyElement<'static>> {
    match &props.state {
        ResumeTaskState::TeleportError {
            current_error,
            is_logging_in,
            errors_to_ignore,
            stash_state,
        } => element! {
            TeleportError(
                current_error: *current_error,
                is_logging_in: *is_logging_in,
                errors_to_ignore: errors_to_ignore.clone(),
                stash_state: stash_state.clone(),
            )
        }
        .into_any(),
        ResumeTaskState::Loading { retrying } => element! {
            View(flex_direction: FlexDirection::Column, padding: 1u32) {
                View(flex_direction: FlexDirection::Row) {
                    Spinner
                    Text(content: "Loading Claude Code sessions…".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                }
                Text(content: if *retrying { "Retrying…".to_string() } else { "Fetching your Claude Code sessions…".to_string() }, dim: true, wrap: TextWrap::NoWrap)
            }
        }
        .into_any(),
        ResumeTaskState::LoadError { error_type, esc_key } => {
            let guidance = render_error_guidance(*error_type);
            element! {
                View(flex_direction: FlexDirection::Column, padding: 1u32) {
                    Text(content: "Error loading Claude Code sessions".to_string(), weight: Weight::Bold, color: Color::Red, wrap: TextWrap::NoWrap)
                    #(Some(guidance))
                    View(flex_direction: FlexDirection::Row) {
                        Text(content: "Press ".to_string(), dim: true, wrap: TextWrap::NoWrap)
                        Text(content: "Ctrl+R".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                        Text(content: " to retry · Press ".to_string(), dim: true, wrap: TextWrap::NoWrap)
                        Text(content: esc_key.clone(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                        Text(content: " to cancel".to_string(), dim: true, wrap: TextWrap::NoWrap)
                    }
                }
            }
            .into_any()
        }
        ResumeTaskState::Empty { current_repo, esc_key } => element! {
            View(flex_direction: FlexDirection::Column, padding: 1u32) {
                View(flex_direction: FlexDirection::Row) {
                    Text(content: "No Claude Code sessions found".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                    #(current_repo.as_ref().map(|repo| element! {
                        Text(content: format!(" for {repo}"), wrap: TextWrap::NoWrap)
                    }))
                }
                View(margin_top: 1u32, flex_direction: FlexDirection::Row) {
                    Text(content: "Press ".to_string(), dim: true, wrap: TextWrap::NoWrap)
                    Text(content: esc_key.clone(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                    Text(content: " to cancel".to_string(), dim: true, wrap: TextWrap::NoWrap)
                }
            }
        }
        .into_any(),
        ResumeTaskState::Ready {
            sessions,
            current_repo,
            rows,
            is_embedded,
            focused_index,
            visible_from_index,
            now_ms,
        } => {
            let max_visible = resume_task_max_visible_options(sessions.len(), *rows, *is_embedded);
            let show_scroll_position = sessions.len() > max_visible;
            let options = resume_task_session_options(sessions, *now_ms);
            let max_time_width = options
                .iter()
                .map(|option| option.label.split(RESUME_TASK_SPACE_BETWEEN_TABLE_COLUMNS).next().unwrap_or("").len())
                .chain(std::iter::once(RESUME_TASK_UPDATED_STRING.len()))
                .max()
                .unwrap_or(RESUME_TASK_UPDATED_STRING.len());
            let focused_index = (*focused_index).min(sessions.len().saturating_sub(1));
            let max_height = max_visible as u32 + RESUME_TASK_LAYOUT_OVERHEAD as u32;

            element! {
                View(flex_direction: FlexDirection::Column, padding: 1u32, height: max_height) {
                    View(flex_direction: FlexDirection::Row) {
                        Text(content: "Select a session to resume".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                        #(if show_scroll_position {
                            Some(element! { Text(content: format!(" ({} of {})", focused_index + 1, sessions.len()), dim: true, wrap: TextWrap::NoWrap) }.into_any())
                        } else { None })
                        #(current_repo.as_ref().map(|repo| element! { Text(content: format!(" ({repo})"), dim: true, wrap: TextWrap::NoWrap) }.into_any()))
                        Text(content: ":".to_string(), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                    }
                    View(flex_direction: FlexDirection::Column, margin_top: 1u32, flex_grow: 1.0f32) {
                        View(margin_left: 2u32) {
                            Text(content: format!("{:<max_time_width$}{RESUME_TASK_SPACE_BETWEEN_TABLE_COLUMNS}Session Title", RESUME_TASK_UPDATED_STRING), weight: Weight::Bold, wrap: TextWrap::NoWrap)
                        }
                        Select(
                            visible_option_count: max_visible,
                            options: options,
                            focused_index: focused_index,
                            visible_from_index: *visible_from_index,
                            layout: SelectLayout::Expanded,
                        )
                    }
                    View(flex_direction: FlexDirection::Row) {
                        Byline {
                            KeyboardShortcutHint(shortcut: "↑/↓".to_string(), action: "select".to_string())
                            KeyboardShortcutHint(shortcut: "Enter".to_string(), action: "confirm".to_string())
                            ConfigurableShortcutHint(
                                action: "confirm:no".to_string(),
                                context: "Confirmation".to_string(),
                                fallback: "Esc".to_string(),
                                description: "cancel".to_string(),
                            )
                        }
                    }
                }
            }
            .into_any()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    fn session(id: &str, title: &str, updated_at: &str, repo: Option<(&str, &str)>) -> CodeSession {
        CodeSession {
            id: id.to_string(),
            title: title.to_string(),
            updated_at: updated_at.to_string(),
            repo: repo.map(|(owner, name)| CodeSessionRepo {
                owner_login: owner.to_string(),
                name: name.to_string(),
                default_branch: None,
            }),
        }
    }

    fn render(state: ResumeTaskState) -> String {
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                ResumeTask(state: state)
            }
        }
        .render(Some(140))
        .to_string()
    }

    #[test]
    fn resume_task_error_type_detection_matches_official_order() {
        assert_eq!(
            determine_resume_task_error_type("network timeout"),
            ResumeTaskLoadErrorType::Network
        );
        assert_eq!(
            determine_resume_task_error_type("403 not authenticated token"),
            ResumeTaskLoadErrorType::Auth
        );
        assert_eq!(
            determine_resume_task_error_type("API rate limit 529"),
            ResumeTaskLoadErrorType::Api
        );
        assert_eq!(
            determine_resume_task_error_type("unknown failure"),
            ResumeTaskLoadErrorType::Other
        );
    }

    #[test]
    fn resume_task_filters_and_sorts_sessions_like_official_loader() {
        let sessions = vec![
            session(
                "old",
                "Old",
                "2026-01-01T00:00:00Z",
                Some(("anthropic", "claude-code")),
            ),
            session(
                "new",
                "New",
                "2026-01-03T00:00:00Z",
                Some(("anthropic", "claude-code")),
            ),
            session(
                "other",
                "Other",
                "2026-01-02T00:00:00Z",
                Some(("other", "repo")),
            ),
        ];
        let filtered = filter_sessions_for_repo(&sessions, Some("anthropic/claude-code"));
        assert_eq!(filtered.len(), 2);
        let sorted = sort_sessions_newest_first(&filtered);
        assert_eq!(
            sorted.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["new", "old"]
        );
    }

    #[test]
    fn resume_task_max_visible_options_matches_official_embedded_and_fullscreen_math() {
        assert_eq!(resume_task_max_visible_options(20, 30, true), 5);
        assert_eq!(resume_task_max_visible_options(20, 12, true), 1);
        assert_eq!(resume_task_max_visible_options(20, 20, false), 12);
        assert_eq!(resume_task_max_visible_options(0, 20, false), 1);
    }

    #[test]
    fn resume_task_session_options_pad_updated_column() {
        let now_ms = chrono::DateTime::parse_from_rfc3339("2026-01-03T00:00:00Z")
            .unwrap()
            .timestamp_millis();
        let sessions = vec![
            session("a", "Alpha", "2026-01-02T23:59:00Z", None),
            session("b", "Beta", "2026-01-01T00:00:00Z", None),
        ];
        let options = resume_task_session_options(&sessions, now_ms);
        assert_eq!(options[0].value, "a");
        assert!(
            options[0].label.contains("1m ago   Alpha"),
            "label={}",
            options[0].label
        );
        assert!(
            options[1].label.contains("2d ago   Beta"),
            "label={}",
            options[1].label
        );
    }

    #[test]
    fn resume_task_renders_loading_error_empty_and_ready_states() {
        let loading = render(ResumeTaskState::Loading { retrying: false });
        assert!(
            loading.contains("Loading Claude Code sessions…"),
            "canvas=\n{loading}"
        );
        assert!(
            loading.contains("Fetching your Claude Code sessions…"),
            "canvas=\n{loading}"
        );

        let error = render(ResumeTaskState::LoadError {
            error_type: ResumeTaskLoadErrorType::Auth,
            esc_key: "Esc".to_string(),
        });
        assert!(
            error.contains("Error loading Claude Code sessions"),
            "canvas=\n{error}"
        );
        assert!(
            error.contains("Teleport requires a Claude account"),
            "canvas=\n{error}"
        );
        assert!(
            error.contains("Press Ctrl+R to retry · Press Esc to cancel"),
            "canvas=\n{error}"
        );

        let empty = render(ResumeTaskState::Empty {
            current_repo: Some("anthropic/claude-code".to_string()),
            esc_key: "Esc".to_string(),
        });
        assert!(
            empty.contains("No Claude Code sessions found for anthropic/claude-code"),
            "canvas=\n{empty}"
        );

        let now_ms = chrono::DateTime::parse_from_rfc3339("2026-01-03T00:00:00Z")
            .unwrap()
            .timestamp_millis();
        let ready = render(ResumeTaskState::Ready {
            sessions: vec![session("a", "Fix MCP", "2026-01-02T00:00:00Z", None)],
            current_repo: Some("anthropic/claude-code".to_string()),
            rows: 20,
            is_embedded: false,
            focused_index: 0,
            visible_from_index: 0,
            now_ms,
        });
        assert!(
            ready.contains("Select a session to resume (anthropic/claude-code):"),
            "canvas=\n{ready}"
        );
        assert!(ready.contains("Updated  Session Title"), "canvas=\n{ready}");
        assert!(ready.contains("1d ago   Fix MCP"), "canvas=\n{ready}");
        assert!(
            ready.contains("↑/↓ to select · Enter to confirm · Esc to cancel"),
            "canvas=\n{ready}"
        );
    }
}
