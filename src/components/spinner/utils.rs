//! Maps to: CC `components/Spinner/utils.ts`.
//! Kept separate from the rendering component so spinner frame selection and
//! color math can be shared by `SpinnerGlyph`, `GlimmerMessage`, and
//! higher-level spinner rows, matching the CC component boundary.

use iocraft::prelude::Color;

const MACOS_DEFAULT_CHARACTERS: [&str; 6] = ["·", "✢", "✳", "✶", "✻", "✽"];
const GHOSTTY_DEFAULT_CHARACTERS: [&str; 6] = ["·", "✢", "✳", "✶", "✻", "*"];
const NON_MACOS_DEFAULT_CHARACTERS: [&str; 6] = ["·", "✢", "*", "✶", "✻", "✽"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RgbColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// Maps to CC `getDefaultCharacters()`.
pub fn get_default_characters() -> &'static [&'static str] {
    if crate::utils::process_env::var("TERM").as_deref() == Some("xterm-ghostty") {
        &GHOSTTY_DEFAULT_CHARACTERS
    } else if cfg!(target_os = "macos") {
        &MACOS_DEFAULT_CHARACTERS
    } else {
        &NON_MACOS_DEFAULT_CHARACTERS
    }
}

/// Maps to CC `DEFAULT_CHARACTERS = getDefaultCharacters()` at module level in
/// `Spinner.tsx:52` and `Spinner/SpinnerGlyph.tsx:11`: computed at import,
/// before settings env applies. `entrypoints/cli.rs` forces it in the startup
/// window.
pub(crate) static DEFAULT_CHARACTERS: std::sync::LazyLock<&'static [&'static str]> =
    std::sync::LazyLock::new(get_default_characters);

/// Maps to CC `SPINNER_FRAMES = [...DEFAULT_CHARACTERS, ...reverse]`.
pub fn spinner_frame(frame: usize) -> &'static str {
    let chars = *DEFAULT_CHARACTERS;
    let cycle_len = chars.len() * 2;
    let idx = frame % cycle_len;
    if idx < chars.len() {
        chars[idx]
    } else {
        chars[cycle_len - idx - 1]
    }
}

pub fn interpolate_color(color1: RgbColor, color2: RgbColor, t: f32) -> RgbColor {
    let t = t.clamp(0.0, 1.0);
    let lerp = |a: u8, b: u8| -> u8 { (a as f32 + (b as f32 - a as f32) * t).round() as u8 };
    RgbColor {
        r: lerp(color1.r, color2.r),
        g: lerp(color1.g, color2.g),
        b: lerp(color1.b, color2.b),
    }
}

pub fn to_rgb_color(color: RgbColor) -> Color {
    Color::Rgb {
        r: color.r,
        g: color.g,
        b: color.b,
    }
}

pub fn color_to_rgb(color: Color) -> Option<RgbColor> {
    match color {
        Color::Rgb { r, g, b } => Some(RgbColor { r, g, b }),
        _ => None,
    }
}

pub fn interpolate_terminal_color(base: Color, target: Color, t: f32) -> Color {
    match (color_to_rgb(base), color_to_rgb(target)) {
        (Some(base_rgb), Some(target_rgb)) => {
            to_rgb_color(interpolate_color(base_rgb, target_rgb, t))
        }
        _ => {
            if t > 0.5 {
                target
            } else {
                base
            }
        }
    }
}

pub fn sine_opacity(time_ms: u64, delay_ms: u64, period_ms: u64) -> f32 {
    if time_ms < delay_ms || period_ms == 0 {
        return 0.0;
    }
    let elapsed = (time_ms - delay_ms) as f32 / period_ms as f32;
    ((elapsed * std::f32::consts::TAU).sin() + 1.0) / 2.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spinner_color_interpolation_matches_official_rounding_shape() {
        assert_eq!(
            interpolate_color(
                RgbColor { r: 0, g: 10, b: 20 },
                RgbColor {
                    r: 100,
                    g: 110,
                    b: 120,
                },
                0.5,
            ),
            RgbColor {
                r: 50,
                g: 60,
                b: 70,
            }
        );
    }
}
