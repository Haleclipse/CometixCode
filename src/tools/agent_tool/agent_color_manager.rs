//! Agent color assignment for subagent UI chrome.
//!
//! Maps to: CC `tools/AgentTool/agentColorManager.ts`.

use crate::utils::theme::ThemeColorKey;

/// Maps to: CC `agentColorManager.ts#AgentColorName`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AgentColorName {
    Red,
    Blue,
    Green,
    Yellow,
    Purple,
    Orange,
    Pink,
    Cyan,
}

/// Maps to: CC `agentColorManager.ts#AGENT_COLORS`.
pub const AGENT_COLORS: [AgentColorName; 8] = [
    AgentColorName::Red,
    AgentColorName::Blue,
    AgentColorName::Green,
    AgentColorName::Yellow,
    AgentColorName::Purple,
    AgentColorName::Orange,
    AgentColorName::Pink,
    AgentColorName::Cyan,
];

/// Rust string-to-enum boundary for CC `AgentColorName` and `AGENT_COLORS`.
/// CC carries these values as a string union; Rust validates before storing.
/// Maps to: CC `agentColorManager.ts:63-65` `AGENT_COLORS.includes(color)` —
/// EXACT lowercase match; CC never trims or case-folds.
pub fn parse_agent_color_name(value: &str) -> Option<AgentColorName> {
    match value {
        "red" => Some(AgentColorName::Red),
        "blue" => Some(AgentColorName::Blue),
        "green" => Some(AgentColorName::Green),
        "yellow" => Some(AgentColorName::Yellow),
        "purple" => Some(AgentColorName::Purple),
        "orange" => Some(AgentColorName::Orange),
        "pink" => Some(AgentColorName::Pink),
        "cyan" => Some(AgentColorName::Cyan),
        _ => None,
    }
}

/// Exact-membership probe over CC `AGENT_COLORS` (agentColorManager.ts:14-23).
pub fn is_valid_agent_color_name(value: &str) -> bool {
    parse_agent_color_name(value).is_some()
}

impl AgentColorName {
    pub fn official_name(self) -> &'static str {
        match self {
            Self::Red => "red",
            Self::Blue => "blue",
            Self::Green => "green",
            Self::Yellow => "yellow",
            Self::Purple => "purple",
            Self::Orange => "orange",
            Self::Pink => "pink",
            Self::Cyan => "cyan",
        }
    }

    /// Maps to: CC `AGENT_COLOR_TO_THEME_COLOR` values (`*_FOR_SUBAGENTS_ONLY`).
    pub fn theme_key(self) -> ThemeColorKey {
        match self {
            Self::Red => ThemeColorKey::AgentRed,
            Self::Blue => ThemeColorKey::AgentBlue,
            Self::Green => ThemeColorKey::AgentGreen,
            Self::Yellow => ThemeColorKey::AgentYellow,
            Self::Purple => ThemeColorKey::AgentPurple,
            Self::Orange => ThemeColorKey::AgentOrange,
            Self::Pink => ThemeColorKey::AgentPink,
            Self::Cyan => ThemeColorKey::AgentCyan,
        }
    }
}

/// Maps to: CC `agentColorManager.ts#AGENT_COLOR_TO_THEME_COLOR`.
pub fn agent_color_to_theme_color(color: AgentColorName) -> ThemeColorKey {
    color.theme_key()
}

/// Maps to: CC `agentColorManager.ts#getAgentColor`.
pub fn get_agent_color(agent_type: &str) -> Option<ThemeColorKey> {
    if agent_type == "general-purpose" {
        return None;
    }
    let map = crate::bootstrap::state::get_agent_color_map()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let existing = map.get(agent_type).copied()?;
    if AGENT_COLORS.contains(&existing) {
        Some(agent_color_to_theme_color(existing))
    } else {
        None
    }
}

/// Maps to: CC `agentColorManager.ts#setAgentColor`.
pub fn set_agent_color(agent_type: impl Into<String>, color: Option<AgentColorName>) {
    let agent_type = agent_type.into();
    let mut map = crate::bootstrap::state::get_agent_color_map()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match color {
        None => {
            map.remove(&agent_type);
        }
        Some(color) if AGENT_COLORS.contains(&color) => {
            map.insert(agent_type, color);
        }
        Some(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn get_set_agent_color_matches_official_semantics() {
        struct AgentColorMapRestore(std::collections::HashMap<String, AgentColorName>);

        impl AgentColorMapRestore {
            fn capture() -> Self {
                Self(
                    crate::bootstrap::state::get_agent_color_map()
                        .read()
                        .unwrap_or_else(|error| error.into_inner())
                        .clone(),
                )
            }
        }

        impl Drop for AgentColorMapRestore {
            fn drop(&mut self) {
                *crate::bootstrap::state::get_agent_color_map()
                    .write()
                    .unwrap_or_else(|error| error.into_inner()) = self.0.clone();
            }
        }

        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _restore = AgentColorMapRestore::capture();
        set_agent_color("reviewer-test", Some(AgentColorName::Blue));
        assert_eq!(
            get_agent_color("reviewer-test"),
            Some(ThemeColorKey::AgentBlue)
        );
        assert_eq!(get_agent_color("general-purpose"), None);
        set_agent_color("reviewer-test", None);
        assert_eq!(get_agent_color("reviewer-test"), None);
    }
}
