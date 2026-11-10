//! Maps to: CC `services/tips/tipRegistry.ts` — spinner tip pool.
//!
//! Ports the always-safe static subset plus `spinnerTipsOverride` custom tips.

use super::tip_history::get_sessions_since_last_shown;
use super::types::Tip;
use crate::utils::settings::get_initial_settings;

fn builtin_tips() -> Vec<Tip> {
    vec![
        Tip {
            id: "memory-command".to_string(),
            content: "Use /memory to view and manage Claude memory".to_string(),
            cooldown_sessions: 15,
        },
        Tip {
            id: "theme-command".to_string(),
            content: "Use /theme to change the color theme".to_string(),
            cooldown_sessions: 20,
        },
        Tip {
            id: "status-line".to_string(),
            content: "Customize your status line with /statusline".to_string(),
            cooldown_sessions: 25,
        },
        Tip {
            id: "prompt-queue".to_string(),
            content: "Hit Enter to queue another message while Claude is working".to_string(),
            cooldown_sessions: 5,
        },
        Tip {
            id: "todo-list".to_string(),
            content: "Ask Claude to use a todo list for complex tasks".to_string(),
            cooldown_sessions: 20,
        },
        Tip {
            id: "permissions".to_string(),
            content: "Manage tool permissions with /permissions".to_string(),
            cooldown_sessions: 10,
        },
        Tip {
            id: "continue".to_string(),
            content: "Use /resume to continue a previous conversation".to_string(),
            cooldown_sessions: 10,
        },
        Tip {
            id: "double-esc".to_string(),
            content: "Press Esc twice to jump into previous messages".to_string(),
            cooldown_sessions: 10,
        },
        Tip {
            id: "shift-tab".to_string(),
            content: "Press Shift+Tab to cycle permission modes".to_string(),
            cooldown_sessions: 10,
        },
        Tip {
            id: "custom-commands".to_string(),
            content: "Create project commands in .nyan/commands/".to_string(),
            cooldown_sessions: 15,
        },
    ]
}

fn custom_tips() -> Vec<Tip> {
    let settings = get_initial_settings();
    let Some(override_settings) = settings.spinner_tips_override.as_ref() else {
        return Vec::new();
    };
    if override_settings.tips.is_empty() {
        return Vec::new();
    }
    override_settings
        .tips
        .iter()
        .enumerate()
        .map(|(index, content)| Tip {
            id: format!("custom-tip-{index}"),
            content: content.clone(),
            cooldown_sessions: 0,
        })
        .collect()
}

/// Maps to: CC `getRelevantTips`.
pub fn get_relevant_tips() -> Vec<Tip> {
    let settings = get_initial_settings();
    let custom = custom_tips();
    let exclude_default = settings
        .spinner_tips_override
        .as_ref()
        .and_then(|override_settings| override_settings.exclude_default)
        .unwrap_or(false);
    if exclude_default && !custom.is_empty() {
        return custom
            .into_iter()
            .filter(|tip| get_sessions_since_last_shown(&tip.id) >= tip.cooldown_sessions)
            .collect();
    }

    builtin_tips()
        .into_iter()
        .chain(custom)
        .filter(|tip| get_sessions_since_last_shown(&tip.id) >= tip.cooldown_sessions)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_tips_do_not_advertise_removed_feedback_commands() {
        let tips = builtin_tips();
        assert_eq!(tips.len(), 10);
        for tip in &tips {
            assert!(!tip.content.contains("/feedback"));
            assert!(!tip.content.contains("/bug"));
        }
        assert!(tips.iter().any(|tip| tip.content.contains("/memory")));
        assert!(tips.iter().any(|tip| tip.content.contains("/resume")));
    }
}
