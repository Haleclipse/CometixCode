//! Maps to: CC `projectOnboardingState.ts`.
//!
//! Project onboarding is the runtime producer for `LogoV2/feedConfigs.tsx`.
//! The read paths inspect the current working directory just like CC. Write
//! paths use `utils::config::save_current_project_config`, which is dry-run by
//! default in Cometix unless `COMETIX_WRITE_ENABLED=1` is explicitly set.

use crate::components::logo_v2::feed_configs::ProjectOnboardingStep;
use crate::utils::config::{
    ProjectConfig, get_current_project_config, save_current_project_config,
};
use crate::utils::process_env::JsTruthy;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub key: String,
    pub text: String,
    pub is_complete: bool,
    pub is_completable: bool,
    pub is_enabled: bool,
}

impl Step {
    pub fn as_feed_step(&self) -> ProjectOnboardingStep {
        ProjectOnboardingStep {
            text: self.text.clone(),
            is_enabled: self.is_enabled,
            is_complete: self.is_complete,
        }
    }
}

pub fn is_dir_empty(path: &Path) -> bool {
    crate::utils::file::is_dir_empty(path)
}

pub fn get_steps_for_path(cwd: &Path) -> Vec<Step> {
    let has_claude_md =
        crate::utils::fs_operations::get_fs_implementation().exists_sync(&cwd.join("CLAUDE.md"));
    let is_workspace_dir_empty = is_dir_empty(cwd);

    vec![
        Step {
            key: "workspace".to_string(),
            text: "Ask Claude to create a new app or clone a repository".to_string(),
            is_complete: false,
            is_completable: true,
            is_enabled: is_workspace_dir_empty,
        },
        Step {
            key: "claudemd".to_string(),
            text: "Run /init to create a CLAUDE.md file with instructions for Claude".to_string(),
            is_complete: has_claude_md,
            is_completable: true,
            is_enabled: !is_workspace_dir_empty,
        },
    ]
}

pub fn get_steps() -> Vec<Step> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    get_steps_for_path(&cwd)
}

pub fn is_project_onboarding_complete_for_steps(steps: &[Step]) -> bool {
    steps
        .iter()
        .filter(|step| step.is_completable && step.is_enabled)
        .all(|step| step.is_complete)
}

pub fn is_project_onboarding_complete_for_path(cwd: &Path) -> bool {
    is_project_onboarding_complete_for_steps(&get_steps_for_path(cwd))
}

pub fn is_project_onboarding_complete() -> bool {
    is_project_onboarding_complete_for_steps(&get_steps())
}

pub fn should_show_project_onboarding_for_config_and_steps(
    project_config: &ProjectConfig,
    steps: &[Step],
    is_demo: bool,
) -> bool {
    if project_config.has_completed_project_onboarding == Some(true)
        || project_config.project_onboarding_seen_count >= 4
        || is_demo
    {
        return false;
    }

    !is_project_onboarding_complete_for_steps(steps)
}

pub fn should_show_project_onboarding_for_path(
    project_config: &ProjectConfig,
    cwd: &Path,
    is_demo: bool,
) -> bool {
    should_show_project_onboarding_for_config_and_steps(
        project_config,
        &get_steps_for_path(cwd),
        is_demo,
    )
}

pub fn should_show_project_onboarding() -> bool {
    let project_config = get_current_project_config();
    let is_demo = crate::utils::process_env::var("IS_DEMO").truthy().is_some();
    should_show_project_onboarding_for_config_and_steps(&project_config, &get_steps(), is_demo)
}

pub fn maybe_mark_project_onboarding_complete() -> anyhow::Result<()> {
    if get_current_project_config().has_completed_project_onboarding == Some(true) {
        return Ok(());
    }

    if is_project_onboarding_complete() {
        save_current_project_config(|current| {
            current.has_completed_project_onboarding = Some(true);
        })?;
    }

    Ok(())
}

pub fn increment_project_onboarding_seen_count() -> anyhow::Result<()> {
    save_current_project_config(|current| {
        current.project_onboarding_seen_count =
            current.project_onboarding_seen_count.saturating_add(1);
    })
}

pub fn feed_steps(steps: &[Step]) -> Vec<ProjectOnboardingStep> {
    steps.iter().map(Step::as_feed_step).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temp_project_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cometix-project-onboarding-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn get_steps_matches_official_empty_workspace_and_claudemd_rules() {
        let dir = temp_project_dir();
        let steps = get_steps_for_path(&dir);
        assert_eq!(steps[0].key, "workspace");
        assert!(steps[0].is_enabled);
        assert!(!steps[0].is_complete);
        assert_eq!(steps[1].key, "claudemd");
        assert!(!steps[1].is_enabled);

        std::fs::write(dir.join("main.rs"), "fn main() {}\n").unwrap();
        let steps = get_steps_for_path(&dir);
        assert!(!steps[0].is_enabled);
        assert!(steps[1].is_enabled);
        assert!(!steps[1].is_complete);

        std::fs::write(dir.join("CLAUDE.md"), "project notes\n").unwrap();
        let steps = get_steps_for_path(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(steps[1].is_complete);
    }

    #[test]
    fn project_onboarding_completion_matches_enabled_completable_steps_only() {
        assert!(!is_project_onboarding_complete_for_steps(&[Step {
            key: "workspace".to_string(),
            text: "x".to_string(),
            is_complete: false,
            is_completable: true,
            is_enabled: true,
        }]));
        assert!(is_project_onboarding_complete_for_steps(&[Step {
            key: "disabled".to_string(),
            text: "x".to_string(),
            is_complete: false,
            is_completable: true,
            is_enabled: false,
        }]));
        assert!(is_project_onboarding_complete_for_steps(&[Step {
            key: "done".to_string(),
            text: "x".to_string(),
            is_complete: true,
            is_completable: true,
            is_enabled: true,
        }]));
    }

    #[test]
    fn should_show_project_onboarding_matches_official_config_gates() {
        let steps = vec![Step {
            key: "workspace".to_string(),
            text: "x".to_string(),
            is_complete: false,
            is_completable: true,
            is_enabled: true,
        }];
        let mut config = ProjectConfig::default();
        assert!(should_show_project_onboarding_for_config_and_steps(
            &config, &steps, false
        ));

        config.has_completed_project_onboarding = Some(true);
        assert!(!should_show_project_onboarding_for_config_and_steps(
            &config, &steps, false
        ));

        config.has_completed_project_onboarding = None;
        config.project_onboarding_seen_count = 4;
        assert!(!should_show_project_onboarding_for_config_and_steps(
            &config, &steps, false
        ));

        config.project_onboarding_seen_count = 0;
        assert!(!should_show_project_onboarding_for_config_and_steps(
            &config, &steps, true
        ));
    }

    #[test]
    fn feed_steps_preserve_official_step_text_and_flags() {
        let dir = temp_project_dir();
        let steps = get_steps_for_path(&dir);
        let feed_steps = feed_steps(&steps);
        assert_eq!(
            feed_steps[0].text,
            "Ask Claude to create a new app or clone a repository"
        );
        assert!(feed_steps[0].is_enabled);
        assert!(!feed_steps[0].is_complete);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
