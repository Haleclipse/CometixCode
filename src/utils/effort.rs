//! Unified reasoning depth (effort) for Nyan.
//!
//! Per design §2:
//! - Four unified levels: `off < medium < high < max`. `off` disables reasoning.
//! - Effort follows the layer that resolves the model: explicit `:effort`,
//!   `inherit` from parent session, profile's configured effort, or model default.
//! - CC legacy `effortLevel` setting, `CLAUDE_CODE_EFFORT_LEVEL` env, launch pinning,
//!   and ultracode are removed.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::constants::figures::{EFFORT_HIGH, EFFORT_LOW, EFFORT_MAX, EFFORT_MEDIUM};

/// Unified thinking / reasoning effort level.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Off,
    #[default]
    Medium,
    High,
    Max,
}

/// The `effort:` frontmatter value of a skill or plugin command. Agents have no
/// such field (their depth is the `:effort` of `model:`), and runtime model
/// selection and provider requests use [`Effort`] only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EffortValue {
    Named(String),
    Numeric(i64),
}

impl EffortValue {
    pub fn as_str(&self) -> String {
        match self {
            Self::Named(value) => value.clone(),
            Self::Numeric(value) => value.to_string(),
        }
    }
}

pub type ModelEffortLevel = Effort;

/// Effort names accepted by the skill and plugin command frontmatter parsers.
pub const EFFORT_LEVELS: &[&str] = &["off", "medium", "high", "max"];

impl Effort {
    pub const ALL: [Effort; 4] = [Effort::Off, Effort::Medium, Effort::High, Effort::Max];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Max => "max",
        }
    }

    pub fn label(self) -> &'static str {
        self.as_str()
    }

    pub fn title_label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Medium => "Medium",
            Self::High => "High",
            Self::Max => "Max",
        }
    }

    pub fn symbol(self) -> &'static str {
        match self {
            Self::Off => EFFORT_LOW,
            Self::Medium => EFFORT_MEDIUM,
            Self::High => EFFORT_HIGH,
            Self::Max => EFFORT_MAX,
        }
    }

    pub fn is_off(self) -> bool {
        self == Self::Off
    }

    pub fn is_thinking(self) -> bool {
        self != Self::Off
    }

    /// Case-insensitive parsing of the four unified effort levels.
    pub fn from_str_loose(s: &str) -> Option<Self> {
        let trimmed = s.trim();
        if trimmed.eq_ignore_ascii_case("off") {
            Some(Self::Off)
        } else if trimmed.eq_ignore_ascii_case("medium") {
            Some(Self::Medium)
        } else if trimmed.eq_ignore_ascii_case("high") {
            Some(Self::High)
        } else if trimmed.eq_ignore_ascii_case("max") {
            Some(Self::Max)
        } else {
            None
        }
    }
}

impl fmt::Display for Effort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Parse a skill or plugin command `effort:` frontmatter value without
/// consulting global effort settings.
pub fn parse_effort_value(value: &str) -> Option<EffortValue> {
    if value.is_empty() {
        return None;
    }
    let lower = value.to_ascii_lowercase();
    if EFFORT_LEVELS.contains(&lower.as_str()) {
        return Some(EffortValue::Named(lower));
    }
    let trimmed = lower.trim_start();
    let (sign, digits) = match trimmed.as_bytes().first().copied() {
        Some(b'-') => (-1_i128, &trimmed[1..]),
        Some(b'+') => (1_i128, &trimmed[1..]),
        _ => (1_i128, trimmed),
    };
    let digits = digits
        .bytes()
        .take_while(u8::is_ascii_digit)
        .collect::<Vec<_>>();
    if digits.is_empty() {
        return None;
    }
    let number = digits.into_iter().try_fold(0_i128, |value, digit| {
        value.checked_mul(10)?.checked_add((digit - b'0') as i128)
    })? * sign;
    i64::try_from(number).ok().map(EffortValue::Numeric)
}

/// Description of each effort level.
pub fn get_effort_level_description(level: &str) -> &'static str {
    match level {
        "off" => "Disable model reasoning/thinking",
        "medium" => "Balanced reasoning for everyday tasks",
        "high" => "Comprehensive reasoning with thorough analysis and testing",
        "max" => "Maximum reasoning depth for complex problems",
        _ => "Balanced reasoning for everyday tasks",
    }
}

pub fn get_effort_description(effort: Effort) -> &'static str {
    get_effort_level_description(effort.as_str())
}

/// Split a model spec in `ref[:effort]` format.
///
/// Per design §1:
/// - ref = profile name | @light | @heavy | @main | inherit.
/// - Only split on the last `:`, and only if the suffix is a valid effort level
///   (so profile names like `qwen3:32b` survive).
/// - If only `:effort` is given (e.g. `:high`), it is equivalent to `inherit:high`.
pub fn parse_model_ref_and_effort(spec: &str) -> (&str, Option<Effort>) {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return ("", None);
    }
    if let Some(idx) = trimmed.rfind(':') {
        let suffix = &trimmed[idx + 1..];
        if let Some(effort) = Effort::from_str_loose(suffix) {
            let prefix = trimmed[..idx].trim();
            let model_ref = if prefix.is_empty() { "inherit" } else { prefix };
            return (model_ref, Some(effort));
        }
    }
    (trimmed, None)
}

pub fn get_displayed_effort_level(_model: &str, app_state_effort: Option<&Effort>) -> &'static str {
    app_state_effort.copied().unwrap_or(Effort::Medium).as_str()
}

pub fn get_effort_suffix(_model: &str, effort: Option<&Effort>) -> String {
    let Some(effort) = effort else {
        return String::new();
    };
    if effort.is_off() {
        " (thinking off)".to_string()
    } else {
        format!(" with {} effort", effort.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_model_ref_and_effort_splits_only_valid_effort_suffixes() {
        assert_eq!(parse_model_ref_and_effort(""), ("", None));
        assert_eq!(
            parse_model_ref_and_effort(":high"),
            ("inherit", Some(Effort::High))
        );
        assert_eq!(
            parse_model_ref_and_effort(":off"),
            ("inherit", Some(Effort::Off))
        );
        assert_eq!(
            parse_model_ref_and_effort("main:medium"),
            ("main", Some(Effort::Medium))
        );
        assert_eq!(
            parse_model_ref_and_effort("@light:max"),
            ("@light", Some(Effort::Max))
        );
        assert_eq!(
            parse_model_ref_and_effort("@heavy:HIGH"),
            ("@heavy", Some(Effort::High))
        );
        assert_eq!(
            parse_model_ref_and_effort("inherit:high"),
            ("inherit", Some(Effort::High))
        );
        // Models with colons in ID must not be split if suffix is not a valid effort
        assert_eq!(parse_model_ref_and_effort("qwen3:32b"), ("qwen3:32b", None));
        assert_eq!(
            parse_model_ref_and_effort("ollama/qwen3:32b:max"),
            ("ollama/qwen3:32b", Some(Effort::Max))
        );
        assert_eq!(
            parse_model_ref_and_effort("custom-profile"),
            ("custom-profile", None)
        );
    }

    #[test]
    fn effort_ordering_and_properties() {
        assert!(Effort::Off < Effort::Medium);
        assert!(Effort::Medium < Effort::High);
        assert!(Effort::High < Effort::Max);

        assert!(Effort::Off.is_off());
        assert!(!Effort::Off.is_thinking());
        assert!(Effort::Medium.is_thinking());
        assert!(Effort::High.is_thinking());
        assert!(Effort::Max.is_thinking());
    }

    #[test]
    fn effort_string_round_trip() {
        for effort in Effort::ALL {
            let str_val = effort.as_str();
            assert_eq!(Effort::from_str_loose(str_val), Some(effort));
            let json = serde_json::to_string(&effort).unwrap();
            assert_eq!(serde_json::from_str::<Effort>(&json).unwrap(), effort);
        }
        assert_eq!(Effort::from_str_loose("unknown"), None);
        assert_eq!(Effort::from_str_loose("low"), None); // only off, medium, high, max
    }
}
