//! Maps to: CC `utils/systemTheme.ts` — terminal dark/light detection for the
//! `auto` theme setting.
//!
//! Detection follows the terminal's actual background colour (an OSC 11
//! query, answered through [`theme_from_osc_color`]) rather than the OS
//! appearance. The detected theme is cached process-wide so callers can
//! resolve `auto` without waiting for that round-trip; the cache is seeded
//! from `$COLORFGBG`. CC 2.1.88's watcher that issues the query
//! (`utils/systemThemeWatcher.ts`) is a generated stub in the source, so no
//! caller updates the cache yet.

use std::sync::Mutex;

use crate::utils::theme::{ThemeName, ThemeSetting};

/// CC `SystemTheme = 'dark' | 'light'`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemTheme {
    Dark,
    Light,
}

impl SystemTheme {
    pub fn theme_name(self) -> ThemeName {
        match self {
            Self::Dark => ThemeName::Dark,
            Self::Light => ThemeName::Light,
        }
    }
}

/// CC `let cachedSystemTheme: SystemTheme | undefined` (module-level).
static CACHED_SYSTEM_THEME: Mutex<Option<SystemTheme>> = Mutex::new(None);

/// Maps to: CC `systemTheme.ts:24-29` `getSystemThemeName` — cached after the
/// first detection.
pub fn get_system_theme_name() -> SystemTheme {
    let mut cached = CACHED_SYSTEM_THEME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *cached.get_or_insert_with(|| detect_from_color_fg_bg().unwrap_or(SystemTheme::Dark))
}

/// Maps to: CC `systemTheme.ts:35-37` `setCachedSystemTheme` — the watcher's
/// write path, so non-component readers stay in sync.
pub fn set_cached_system_theme(theme: SystemTheme) {
    *CACHED_SYSTEM_THEME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(theme);
}

#[cfg(test)]
pub(crate) fn reset_cached_system_theme_for_testing() {
    *CACHED_SYSTEM_THEME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

/// Maps to: CC `systemTheme.ts:42-47` `resolveThemeSetting`.
pub fn resolve_theme_setting(setting: ThemeSetting) -> ThemeName {
    match setting {
        ThemeSetting::Auto => get_system_theme_name().theme_name(),
        ThemeSetting::Named(name) => name,
    }
}

/// Maps to: CC `systemTheme.ts:60-66` `themeFromOscColor` — ITU-R BT.709
/// relative luminance, split at the midpoint. `None` for formats it does not
/// recognise, so callers can fall back.
pub fn theme_from_osc_color(data: &str) -> Option<SystemTheme> {
    let (r, g, b) = parse_osc_rgb(data)?;
    let luminance = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    Some(if luminance > 0.5 {
        SystemTheme::Light
    } else {
        SystemTheme::Dark
    })
}

/// Maps to: CC `systemTheme.ts:70-94` `parseOscRgb`: `rgb:R/G/B` (or `rgba:`,
/// alpha ignored) with 1–4 hex digits per component, or `#RRGGBB` /
/// `#RRRRGGGGBBBB` split into three equal runs.
fn parse_osc_rgb(data: &str) -> Option<(f64, f64, f64)> {
    let lower = data.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("rgba:")
        .or_else(|| lower.strip_prefix("rgb:"));
    if let Some(rest) = rest {
        // `/^rgba?:([0-9a-f]{1,4})\/([0-9a-f]{1,4})\/([0-9a-f]{1,4})/i` —
        // anchored at the start only, so anything after the third run is
        // ignored.
        let mut components = [0.0; 3];
        let mut remaining = rest;
        for (index, component) in components.iter_mut().enumerate() {
            let digits = remaining
                .bytes()
                .take(4)
                .take_while(u8::is_ascii_hexdigit)
                .count();
            if digits == 0 {
                return None;
            }
            *component = hex_component(&remaining[..digits]);
            remaining = &remaining[digits..];
            if index < 2 {
                remaining = remaining.strip_prefix('/')?;
            }
        }
        return Some((components[0], components[1], components[2]));
    }
    let hex = lower.strip_prefix('#')?;
    if hex.is_empty() || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) || hex.len() % 3 != 0 {
        return None;
    }
    let n = hex.len() / 3;
    Some((
        hex_component(&hex[..n]),
        hex_component(&hex[n..2 * n]),
        hex_component(&hex[2 * n..]),
    ))
}

/// Maps to: CC `systemTheme.ts:97-100` `hexComponent` — a 1–4 digit hex run
/// normalised to [0, 1].
fn hex_component(hex: &str) -> f64 {
    let max = 16f64.powi(hex.len() as i32) - 1.0;
    // JS `parseInt(hex, 16)` over hex digits only (the callers check), in
    // floating point so an over-long `#` run stays finite as it does in JS.
    let value = hex
        .chars()
        .filter_map(|digit| digit.to_digit(16))
        .fold(0.0, |acc, digit| acc * 16.0 + f64::from(digit));
    value / max
}

/// Maps to: CC `systemTheme.ts:109-119` `detectFromColorFgBg` — `$COLORFGBG`
/// is `fg;bg` (or `fg;other;bg`) in ANSI colour indices; rxvt convention:
/// bg 0–6 and 8 are dark, 7 and 9–15 light.
fn detect_from_color_fg_bg() -> Option<SystemTheme> {
    // `process.env` decodes lossily rather than dropping a non-UTF-8 value.
    let colorfgbg = crate::utils::process_env::var_os("COLORFGBG")?.to_string_lossy().into_owned();
    if colorfgbg.is_empty() {
        return None;
    }
    let bg = colorfgbg.split(';').next_back()?;
    if bg.is_empty() {
        return None;
    }
    let bg_num = js_number(bg)?;
    if bg_num.fract() != 0.0 || !(0.0..=15.0).contains(&bg_num) {
        return None;
    }
    Some(if bg_num <= 6.0 || bg_num == 8.0 {
        SystemTheme::Dark
    } else {
        SystemTheme::Light
    })
}

/// JS `Number(string)` for the forms `detectFromColorFgBg` can meet: surrounding
/// whitespace is ignored and all-whitespace is 0, decimal with sign, fraction
/// and exponent, `0x`/`0o`/`0b` prefixes, `Infinity`. `None` is JS `NaN`.
fn js_number(value: &str) -> Option<f64> {
    let trimmed = value.trim_matches(|ch: char| (ch.is_whitespace() && ch != '\u{85}') || ch == '\u{feff}');
    if trimmed.is_empty() {
        return Some(0.0);
    }
    for (prefix, radix) in [("0x", 16), ("0X", 16), ("0o", 8), ("0O", 8), ("0b", 2), ("0B", 2)] {
        if let Some(digits) = trimmed.strip_prefix(prefix) {
            // Digits only: JS reads `0x+7` as NaN, where `from_str_radix`
            // would accept the sign.
            if digits.is_empty() || !digits.chars().all(|digit| digit.is_digit(radix)) {
                return None;
            }
            return Some(
                digits
                    .chars()
                    .filter_map(|digit| digit.to_digit(radix))
                    .fold(0.0, |acc, digit| acc * f64::from(radix) + f64::from(digit)),
            );
        }
    }
    match trimmed {
        "Infinity" | "+Infinity" => return Some(f64::INFINITY),
        "-Infinity" => return Some(f64::NEG_INFINITY),
        _ => {}
    }
    // Rust also accepts `inf`, `infinity` and `nan`, which JS reads as NaN.
    if !trimmed
        .bytes()
        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.' | b'e' | b'E'))
    {
        return None;
    }
    trimmed.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn theme_from_osc_color_reads_xparsecolor_forms_by_luminance() {
        // CC systemTheme.ts:60-94.
        assert_eq!(theme_from_osc_color("rgb:ffff/ffff/ffff"), Some(SystemTheme::Light));
        assert_eq!(theme_from_osc_color("rgb:0000/0000/0000"), Some(SystemTheme::Dark));
        assert_eq!(theme_from_osc_color("RGB:1e/1e/2e"), Some(SystemTheme::Dark));
        assert_eq!(theme_from_osc_color("rgba:f/f/f/0"), Some(SystemTheme::Light));
        // Green carries most of the weight: pure green is light, pure blue dark.
        assert_eq!(theme_from_osc_color("rgb:0/ff/0"), Some(SystemTheme::Light));
        assert_eq!(theme_from_osc_color("rgb:0/0/ff"), Some(SystemTheme::Dark));
        assert_eq!(theme_from_osc_color("#ffffff"), Some(SystemTheme::Light));
        assert_eq!(theme_from_osc_color("#000000000000"), Some(SystemTheme::Dark));
        // Anchored at the start only: trailing text after the third run is ignored.
        assert_eq!(theme_from_osc_color("rgb:ff/ff/ff\u{7}"), Some(SystemTheme::Light));
        assert_eq!(theme_from_osc_color("rgb:fffff/0/0"), None);
        assert_eq!(theme_from_osc_color("#fffff"), None);
        assert_eq!(theme_from_osc_color("#"), None);
        assert_eq!(theme_from_osc_color("cmyk:0/0/0/0"), None);
    }

    #[test]
    fn system_theme_seeds_from_colorfgbg_and_caches() {
        // CC systemTheme.ts:24-29,109-119.
        let _lock = TEST_ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let seed = |value: &str| {
            let guard = EnvVarGuard::set("COLORFGBG", value);
            reset_cached_system_theme_for_testing();
            guard
        };
        for (value, expected) in [
            ("15;0", SystemTheme::Dark),
            ("0;15", SystemTheme::Light),
            ("0;default;7", SystemTheme::Light),
            ("15;8", SystemTheme::Dark),
            ("15; 9 ", SystemTheme::Light),
            ("15;0x7", SystemTheme::Light),
            // JS `Number("0x+7")` is NaN.
            ("15;0x+7", SystemTheme::Dark),
            // Not an index 0-15: no hint, so the default.
            ("15;16", SystemTheme::Dark),
            ("15;7.5", SystemTheme::Dark),
            ("15;", SystemTheme::Dark),
            ("15;nan", SystemTheme::Dark),
        ] {
            let _env = seed(value);
            assert_eq!(get_system_theme_name(), expected, "COLORFGBG={value:?}");
        }
        {
            let _env = EnvVarGuard::unset("COLORFGBG");
            reset_cached_system_theme_for_testing();
            assert_eq!(get_system_theme_name(), SystemTheme::Dark);
        }
        // Cached: a later env change does not move it; the watcher's write does.
        let light = seed("0;15");
        assert_eq!(get_system_theme_name(), SystemTheme::Light);
        drop(light);
        let _dark = EnvVarGuard::set("COLORFGBG", "15;0");
        assert_eq!(get_system_theme_name(), SystemTheme::Light);
        set_cached_system_theme(SystemTheme::Dark);
        assert_eq!(resolve_theme_setting(ThemeSetting::Auto), ThemeName::Dark);
        assert_eq!(
            resolve_theme_setting(ThemeSetting::Named(ThemeName::LightAnsi)),
            ThemeName::LightAnsi
        );
        reset_cached_system_theme_for_testing();
    }
}
