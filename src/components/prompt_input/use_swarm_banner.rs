//! Maps to: CC `components/PromptInput/useSwarmBanner.ts:1-156`.
//!
//! This typed projection keeps AppState/process/backend detection outside the
//! hook boundary while preserving the official precedence rules.

use crate::tools::agent_tool::agent_color_manager::parse_agent_color_name;
use crate::utils::theme::ThemeColorKey;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwarmBannerInfo {
    pub text: String,
    pub bg_color: ThemeColorKey,
}

#[derive(Clone, Debug, Default)]
pub struct SwarmBannerInput {
    pub is_external_teammate: bool,
    pub teammate_name: Option<String>,
    pub teammate_team_name: Option<String>,
    pub teammate_color: Option<String>,
    pub has_teammates: bool,
    pub inside_tmux: Option<bool>,
    pub in_process_mode: bool,
    pub native_panes: bool,
    pub viewed_teammate_name: Option<String>,
    pub viewed_teammate_color: Option<String>,
    pub swarm_socket_name: Option<String>,
    pub background_agent_name: Option<String>,
    pub background_agent_description: Option<String>,
    pub background_agent_color: Option<String>,
    pub standalone_name: Option<String>,
    pub standalone_color: Option<String>,
    pub cli_agent: Option<String>,
    pub cli_agent_color: Option<String>,
}

fn color(value: Option<&str>, fallback: ThemeColorKey) -> ThemeColorKey {
    value
        .and_then(parse_agent_color_name)
        .map(|color| color.theme_key())
        .unwrap_or(fallback)
}

pub fn swarm_banner(input: &SwarmBannerInput) -> Option<SwarmBannerInfo> {
    if input.is_external_teammate
        && let (Some(name), Some(_team)) = (&input.teammate_name, &input.teammate_team_name)
    {
        return Some(SwarmBannerInfo {
            text: format!("@{name}"),
            bg_color: color(input.teammate_color.as_deref(), ThemeColorKey::AgentCyan),
        });
    }
    if input.has_teammates {
        let viewed_color = color(
            input.viewed_teammate_color.as_deref(),
            ThemeColorKey::AgentCyan,
        );
        if input.inside_tmux == Some(false) && !input.in_process_mode && !input.native_panes {
            return Some(SwarmBannerInfo {
                text: format!(
                    "View teammates: `tmux -L {} a`",
                    input.swarm_socket_name.as_deref().unwrap_or("claude-swarm")
                ),
                bg_color: viewed_color,
            });
        }
        if (input.inside_tmux == Some(true) || input.in_process_mode || input.native_panes)
            && input.viewed_teammate_name.is_some()
        {
            return Some(SwarmBannerInfo {
                text: format!("@{}", input.viewed_teammate_name.as_deref().unwrap()),
                bg_color: viewed_color,
            });
        }
    }
    if input.background_agent_name.is_some() || input.background_agent_description.is_some() {
        return Some(SwarmBannerInfo {
            text: input
                .background_agent_name
                .as_ref()
                .map(|name| format!("@{name}"))
                .or_else(|| input.background_agent_description.clone())
                .unwrap_or_default(),
            bg_color: color(
                input.background_agent_color.as_deref(),
                ThemeColorKey::AgentCyan,
            ),
        });
    }
    if input
        .standalone_name
        .as_deref()
        .is_some_and(|name| !name.is_empty())
        || input
            .standalone_color
            .as_deref()
            .is_some_and(|color| !color.is_empty())
    {
        return Some(SwarmBannerInfo {
            text: input.standalone_name.clone().unwrap_or_default(),
            bg_color: color(input.standalone_color.as_deref(), ThemeColorKey::AgentCyan),
        });
    }
    input.cli_agent.as_ref().map(|name| SwarmBannerInfo {
        text: name.clone(),
        bg_color: color(
            input.cli_agent_color.as_deref(),
            ThemeColorKey::PromptBorder,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn precedence_covers_teammate_attach_view_background_standalone_and_cli() {
        let mut input = SwarmBannerInput {
            cli_agent: Some("cli".to_string()),
            standalone_name: Some("solo".to_string()),
            background_agent_name: Some("worker".to_string()),
            has_teammates: true,
            inside_tmux: Some(false),
            swarm_socket_name: Some("socket".to_string()),
            ..Default::default()
        };
        assert!(
            swarm_banner(&input)
                .unwrap()
                .text
                .contains("tmux -L socket")
        );
        input.inside_tmux = Some(true);
        input.viewed_teammate_name = Some("mate".to_string());
        assert_eq!(swarm_banner(&input).unwrap().text, "@mate");
        input.has_teammates = false;
        assert_eq!(swarm_banner(&input).unwrap().text, "@worker");
        input.background_agent_name = None;
        input.background_agent_description = None;
        assert_eq!(swarm_banner(&input).unwrap().text, "solo");
        input.standalone_name = None;
        assert_eq!(swarm_banner(&input).unwrap().text, "cli");
    }
    #[test]
    fn standalone_reset_matches_official_truthy_name_and_color() {
        // CC useSwarmBanner.ts:127-133: standaloneName || standaloneColor.
        let mut input = SwarmBannerInput {
            standalone_name: Some(String::new()),
            standalone_color: None,
            ..Default::default()
        };
        assert!(swarm_banner(&input).is_none());
        input.standalone_color = Some("red".into());
        assert_eq!(swarm_banner(&input).unwrap().text, "");
        input.standalone_color = None;
        input.standalone_name = Some("named".into());
        assert_eq!(
            swarm_banner(&input).unwrap().bg_color,
            ThemeColorKey::AgentCyan
        );
        input.standalone_name = Some(String::new());
        input.cli_agent = Some("cli".into());
        assert_eq!(swarm_banner(&input).unwrap().text, "cli");
    }
}
