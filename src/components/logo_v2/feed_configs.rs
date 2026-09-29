//! Maps to: CC `components/LogoV2/feedConfigs.tsx`.
//!
//! These pure helpers mirror the official feed-shaping functions. Runtime
//! producers such as release-note fetch, referral credit lookup, and project
//! onboarding state persistence remain outside this module, matching the CC
//! separation between `feedConfigs.tsx` and `LogoV2.tsx`.

use super::feed::{FeedConfig, FeedCustomContent, FeedCustomLineTone, FeedLine};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecentActivityEntry {
    pub summary: Option<String>,
    pub first_prompt: Option<String>,
    pub timestamp: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectOnboardingStep {
    pub text: String,
    pub is_enabled: bool,
    pub is_complete: bool,
}

pub fn create_recent_activity_feed(activities: &[RecentActivityEntry]) -> FeedConfig {
    let lines = activities
        .iter()
        .map(|activity| {
            let description = activity
                .summary
                .as_deref()
                .filter(|summary| *summary != "No prompt")
                .or(activity.first_prompt.as_deref())
                .unwrap_or("")
                .to_string();
            FeedLine {
                text: description,
                timestamp: activity.timestamp.clone(),
            }
        })
        .collect::<Vec<_>>();

    FeedConfig {
        title: "Recent activity".to_string(),
        footer: (!lines.is_empty()).then(|| "/resume for more".to_string()),
        empty_message: Some("No recent activity".to_string()),
        lines,
        custom_content: None,
    }
}

pub fn create_whats_new_feed(release_notes: &[String]) -> FeedConfig {
    let lines = release_notes
        .iter()
        .map(|note| FeedLine::text(note.clone()))
        .collect::<Vec<_>>();

    FeedConfig {
        title: "What's new".to_string(),
        footer: (!lines.is_empty()).then(|| "/release-notes for more".to_string()),
        empty_message: Some("Check the Claude Code changelog for updates".to_string()),
        lines,
        custom_content: None,
    }
}

pub fn create_project_onboarding_feed(steps: &[ProjectOnboardingStep]) -> FeedConfig {
    let mut enabled_steps = steps
        .iter()
        .filter(|step| step.is_enabled)
        .cloned()
        .collect::<Vec<_>>();
    enabled_steps.sort_by_key(|step| step.is_complete);

    let mut lines = enabled_steps
        .into_iter()
        .map(|step| {
            let checkmark = if step.is_complete {
                format!("{} ", crate::constants::figures::MAIN_SYMBOLS.tick)
            } else {
                String::new()
            };
            FeedLine::text(format!("{checkmark}{}", step.text))
        })
        .collect::<Vec<_>>();

    if launched_from_home_directory() {
        lines.push(FeedLine::text(
            "Note: You have launched claude in your home directory. For the best experience, launch it in a project directory instead.",
        ));
    }

    FeedConfig {
        title: "Tips for getting started".to_string(),
        lines,
        footer: None,
        empty_message: None,
        custom_content: None,
    }
}

pub fn create_guest_passes_feed(reward_text: Option<&str>) -> FeedConfig {
    let subtitle = reward_text
        .map(|reward| format!("Share Claude Code and earn {reward} of extra usage"))
        .unwrap_or_else(|| "Share Claude Code with friends".to_string());

    FeedConfig {
        title: "3 guest passes".to_string(),
        lines: Vec::new(),
        footer: Some("/passes".to_string()),
        empty_message: None,
        custom_content: Some(FeedCustomContent {
            lines: vec![FeedLine::text("[✻] [✻] [✻]"), FeedLine::text(subtitle)],
            width: 48,
            line_tones: vec![FeedCustomLineTone::Claude, FeedCustomLineTone::Dim],
            margin_y_first_line: true,
        }),
    }
}

fn launched_from_home_directory() -> bool {
    let Ok(cwd) = std::env::current_dir() else {
        return false;
    };
    let Some(home) = crate::utils::process_env::var_os("HOME") else {
        return false;
    };
    cwd == std::path::PathBuf::from(home)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_activity_feed_matches_official_summary_first_prompt_and_footer() {
        let feed = create_recent_activity_feed(&[
            RecentActivityEntry {
                summary: Some("Fix parser".to_string()),
                first_prompt: Some("ignored".to_string()),
                timestamp: Some("1h ago".to_string()),
            },
            RecentActivityEntry {
                summary: Some("No prompt".to_string()),
                first_prompt: Some("Initial request".to_string()),
                timestamp: None,
            },
        ]);

        assert_eq!(feed.title, "Recent activity");
        assert_eq!(feed.lines[0].text, "Fix parser");
        assert_eq!(feed.lines[0].timestamp.as_deref(), Some("1h ago"));
        assert_eq!(feed.lines[1].text, "Initial request");
        assert_eq!(feed.footer.as_deref(), Some("/resume for more"));
        assert_eq!(feed.empty_message.as_deref(), Some("No recent activity"));
    }

    #[test]
    fn whats_new_feed_matches_official_footer_and_empty_message() {
        let empty = create_whats_new_feed(&[]);
        assert_eq!(empty.title, "What's new");
        assert!(empty.footer.is_none());
        assert_eq!(
            empty.empty_message.as_deref(),
            Some("Check the Claude Code changelog for updates")
        );

        let populated = create_whats_new_feed(&["Added feature".to_string()]);
        assert_eq!(populated.lines[0].text, "Added feature");
        assert_eq!(populated.footer.as_deref(), Some("/release-notes for more"));
    }

    #[test]
    fn project_onboarding_feed_filters_sorts_and_marks_completed_steps() {
        let feed = create_project_onboarding_feed(&[
            ProjectOnboardingStep {
                text: "Completed".to_string(),
                is_enabled: true,
                is_complete: true,
            },
            ProjectOnboardingStep {
                text: "Pending".to_string(),
                is_enabled: true,
                is_complete: false,
            },
            ProjectOnboardingStep {
                text: "Disabled".to_string(),
                is_enabled: false,
                is_complete: false,
            },
        ]);

        assert_eq!(feed.title, "Tips for getting started");
        assert_eq!(feed.lines[0].text, "Pending");
        assert_eq!(feed.lines[1].text, "✔ Completed");
        assert!(!feed.lines.iter().any(|line| line.text == "Disabled"));
    }

    #[test]
    fn guest_passes_feed_preserves_official_custom_content_shape() {
        let feed = create_guest_passes_feed(Some("$5"));
        assert_eq!(feed.title, "3 guest passes");
        assert_eq!(feed.footer.as_deref(), Some("/passes"));
        let custom = feed.custom_content.expect("custom content");
        assert_eq!(custom.width, 48);
        assert_eq!(custom.lines[0].text, "[✻] [✻] [✻]");
        assert_eq!(custom.line_tones[0], FeedCustomLineTone::Claude);
        assert_eq!(custom.line_tones[1], FeedCustomLineTone::Dim);
        assert!(custom.margin_y_first_line);
        assert_eq!(
            custom.lines[1].text,
            "Share Claude Code and earn $5 of extra usage"
        );
    }
}
