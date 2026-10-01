//! Official-shaped syntax theme seam for StructuredDiff color rendering.
//! Maps to `components/StructuredDiff/colorDiff.ts` and the native
//! `color-diff-napi.getSyntaxTheme` boundary. This module only resolves theme
//! metadata in memory; it does not write settings or session files.

use crate::utils::theme::Theme;
use iocraft::Color;
use std::{collections::HashMap, sync::OnceLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyntaxHighlightTheme {
    Dark,
    Light,
    Ansi,
    Named(&'static str),
}

impl SyntaxHighlightTheme {
    pub fn from_theme(theme: Theme) -> Self {
        if is_ansi_theme(theme) {
            return Self::Ansi;
        }
        if matches!(theme.text, Color::Black | Color::DarkGrey)
            || theme.text == crate::utils::theme::LIGHT.text
            || theme.text == crate::utils::theme::LIGHT_DALTONIZED.text
        {
            Self::Light
        } else {
            Self::Dark
        }
    }

    pub fn from_theme_name(theme_name: &str) -> Self {
        match default_syntax_theme_name(theme_name) {
            "ansi" => Self::Ansi,
            "Monokai Extended" => Self::Dark,
            _ => Self::Light,
        }
    }

    pub fn from_bat_theme_name(theme_name: &str) -> Self {
        match theme_name {
            "ansi" | "ansi-dark" | "ansi-light" => Self::Ansi,
            "Monokai Extended" => Self::Dark,
            "GitHub" => Self::Light,
            _ => Self::Named(intern_syntax_theme_name(theme_name)),
        }
    }

    pub fn bat_theme_name(self) -> &'static str {
        match self {
            Self::Dark => "Monokai Extended",
            Self::Light => "GitHub",
            // Display/env name stays `ansi`; rendering uses official ANSI_SCOPES
            // (bright 8–15), not bat's stock 0–7 `ansi` theme.
            Self::Ansi => "ansi",
            Self::Named(name) => name,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxTheme {
    pub theme: String,
    pub source: Option<&'static str>,
}

impl SyntaxTheme {
    pub fn highlight_theme(&self) -> SyntaxHighlightTheme {
        SyntaxHighlightTheme::from_bat_theme_name(&self.theme)
    }
}

pub fn default_syntax_theme_name(theme_name: &str) -> &'static str {
    let normalized = theme_name.to_ascii_lowercase();
    if normalized.contains("ansi") {
        "ansi"
    } else if normalized.contains("dark") {
        "Monokai Extended"
    } else {
        "GitHub"
    }
}

pub fn get_syntax_theme(theme_name: &str) -> SyntaxTheme {
    let claude_code = crate::utils::process_env::var("CLAUDE_CODE_SYNTAX_HIGHLIGHT");
    let bat_theme = crate::utils::process_env::var("BAT_THEME");
    get_syntax_theme_with_env(theme_name, claude_code.as_deref(), bat_theme.as_deref())
}

pub fn get_syntax_theme_with_env(
    theme_name: &str,
    claude_code_syntax_highlight: Option<&str>,
    bat_theme: Option<&str>,
) -> SyntaxTheme {
    if let Some(value) = claude_code_syntax_highlight {
        if !is_defined_falsy_value(value) && !value.is_empty() {
            return SyntaxTheme {
                theme: value.to_string(),
                source: Some("CLAUDE_CODE_SYNTAX_HIGHLIGHT"),
            };
        }
    }
    if let Some(value) = bat_theme {
        if !value.is_empty() {
            return SyntaxTheme {
                theme: value.to_string(),
                source: Some("BAT_THEME"),
            };
        }
    }
    SyntaxTheme {
        theme: default_syntax_theme_name(theme_name).to_string(),
        source: None,
    }
}

pub fn syntax_highlighting_disabled_by_env() -> Option<String> {
    let claude_code = crate::utils::process_env::var("CLAUDE_CODE_SYNTAX_HIGHLIGHT");
    syntax_highlighting_disabled_by_env_value(claude_code.as_deref())
}

pub fn syntax_highlighting_disabled_by_env_value(value: Option<&str>) -> Option<String> {
    value
        .filter(|value| is_defined_falsy_value(value))
        .map(ToString::to_string)
}

fn is_defined_falsy_value(value: &str) -> bool {
    if value.is_empty() {
        return false;
    }
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "no" | "off"
    )
}

fn intern_syntax_theme_name(name: &str) -> &'static str {
    static INTERNED: OnceLock<std::sync::Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    let mut interned = INTERNED
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
        .lock()
        .expect("syntax theme intern cache mutex should not be poisoned");
    if let Some(value) = interned.get(name) {
        return *value;
    }
    let leaked = Box::leak(name.to_string().into_boxed_str());
    interned.insert(name.to_string(), leaked);
    leaked
}

fn is_ansi_theme(theme: Theme) -> bool {
    let dark = crate::utils::theme::DARK_ANSI;
    let light = crate::utils::theme::LIGHT_ANSI;
    (theme.text == dark.text
        && theme.diff_added == dark.diff_added
        && theme.diff_removed == dark.diff_removed
        && theme.diff_added_word == dark.diff_added_word
        && theme.diff_removed_word == dark.diff_removed_word)
        || (theme.text == light.text
            && theme.diff_added == light.diff_added
            && theme.diff_removed == light.diff_removed
            && theme.diff_added_word == light.diff_added_word
            && theme.diff_removed_word == light.diff_removed_word)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    #[test]
    fn syntax_highlight_theme_default_names_match_official_get_syntax_theme() {
        assert_eq!(default_syntax_theme_name("dark"), "Monokai Extended");
        assert_eq!(
            default_syntax_theme_name("dark-daltonized"),
            "Monokai Extended"
        );
        assert_eq!(default_syntax_theme_name("light"), "GitHub");
        assert_eq!(default_syntax_theme_name("light-daltonized"), "GitHub");
        assert_eq!(default_syntax_theme_name("dark-ansi"), "ansi");
        assert_eq!(default_syntax_theme_name("light-ansi"), "ansi");
    }

    #[test]
    fn syntax_highlight_theme_maps_official_theme_names_including_ansi() {
        assert_eq!(
            SyntaxHighlightTheme::from_theme(theme::DARK),
            SyntaxHighlightTheme::Dark
        );
        assert_eq!(
            SyntaxHighlightTheme::from_theme(theme::LIGHT),
            SyntaxHighlightTheme::Light
        );
        assert_eq!(
            SyntaxHighlightTheme::from_theme(theme::DARK_DALTONIZED),
            SyntaxHighlightTheme::Dark
        );
        assert_eq!(
            SyntaxHighlightTheme::from_theme(theme::LIGHT_DALTONIZED),
            SyntaxHighlightTheme::Light
        );
        assert_eq!(
            SyntaxHighlightTheme::from_theme(theme::DARK_ANSI),
            SyntaxHighlightTheme::Ansi
        );
        assert_eq!(
            SyntaxHighlightTheme::from_theme(theme::LIGHT_ANSI),
            SyntaxHighlightTheme::Ansi
        );
    }

    #[test]
    fn get_syntax_theme_reports_env_source_without_writing_settings() {
        let syntax = get_syntax_theme_with_env("light", Some("TwoDark"), Some("GitHub"));
        assert_eq!(syntax.theme, "TwoDark");
        assert_eq!(syntax.source, Some("CLAUDE_CODE_SYNTAX_HIGHLIGHT"));
        assert_eq!(
            syntax.highlight_theme(),
            SyntaxHighlightTheme::Named("TwoDark")
        );
        assert_eq!(
            syntax_highlighting_disabled_by_env_value(Some("TwoDark")),
            None
        );

        let syntax = get_syntax_theme_with_env("dark", None, Some("ansi"));
        assert_eq!(syntax.theme, "ansi");
        assert_eq!(syntax.source, Some("BAT_THEME"));
        assert_eq!(syntax.highlight_theme(), SyntaxHighlightTheme::Ansi);

        let syntax = get_syntax_theme_with_env("dark-ansi", Some("false"), Some("GitHub"));
        assert_eq!(syntax.theme, "GitHub");
        assert_eq!(syntax.source, Some("BAT_THEME"));
        assert_eq!(
            syntax_highlighting_disabled_by_env_value(Some("false")),
            Some("false".to_string())
        );
    }
}
