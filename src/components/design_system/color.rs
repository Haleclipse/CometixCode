//! Maps to: CC `components/design-system/color.ts`.
//! Rust callers already resolve theme keys to `iocraft::Color`; this helper
//! mirrors the official curried colorizer for raw ANSI-formatted snippets.

use iocraft::prelude::Color;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorType {
    Foreground,
    Background,
}

/// Maps to: CC `components/design-system/color.ts#color` delegation to
/// `ink/colorize.ts#colorize`. Theme strings have already been resolved to
/// iocraft Color by the caller. This is only a color representation adapter;
/// the existing Chalk dependency owns level detection, downgrading, nested
/// resets and newline handling.
pub fn colorize(text: &str, color: Option<Color>, color_type: ColorType) -> String {
    use chalk::{Chalk, NamedColor};
    let Some(color) = color else {
        return text.to_owned();
    };
    let chalk = Chalk::new();
    let background = color_type == ColorType::Background;
    let style = match color {
        Color::Reset => return text.to_owned(),
        Color::Rgb { r, g, b } => {
            if background {
                chalk.bg_rgb(r, g, b)
            } else {
                chalk.rgb(r, g, b)
            }
        }
        Color::AnsiValue(value) => {
            // CC's Chalk ansi256 model emits the explicit palette code at
            // every nonzero level. Reuse the existing builder at level 2 to
            // preserve that contract; level 0 must still suppress styling.
            let chalk = Chalk::with_level(if chalk.level() == 0 { 0 } else { 2 });
            if background {
                chalk.bg_ansi256(value)
            } else {
                chalk.ansi256(value)
            }
        }
        named => {
            let named = match named {
                Color::Black => NamedColor::Black,
                Color::DarkRed => NamedColor::Red,
                Color::DarkGreen => NamedColor::Green,
                Color::DarkYellow => NamedColor::Yellow,
                Color::DarkBlue => NamedColor::Blue,
                Color::DarkMagenta => NamedColor::Magenta,
                Color::DarkCyan => NamedColor::Cyan,
                Color::Grey => NamedColor::White,
                Color::DarkGrey => NamedColor::BlackBright,
                Color::Red => NamedColor::RedBright,
                Color::Green => NamedColor::GreenBright,
                Color::Yellow => NamedColor::YellowBright,
                Color::Blue => NamedColor::BlueBright,
                Color::Magenta => NamedColor::MagentaBright,
                Color::Cyan => NamedColor::CyanBright,
                Color::White => NamedColor::WhiteBright,
                _ => unreachable!("non-named colors handled above"),
            };
            if background {
                chalk.bg_color(named)
            } else {
                chalk.color(named)
            }
        }
    };
    style.apply(text)
}

/// Maps to: CC `components/design-system/color.ts:9-32#color`.
pub fn color(color: Option<Color>, color_type: ColorType) -> impl Fn(&str) -> String {
    move |text| colorize(text, color, color_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_matches_official_bun_levels_named_rgb_and_nested_styles() {
        // Real Bun execution of CC ink/colorize.ts (2026-09-13), including
        // all 16 names, RGB downgrade, explicit palette, newline and embedded resets.
        let cases: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/oracles/terminal-setup-0913/color-oracle.json"
        )))
        .unwrap();
        let colors = [
            Color::Black,
            Color::DarkRed,
            Color::DarkGreen,
            Color::DarkYellow,
            Color::DarkBlue,
            Color::DarkMagenta,
            Color::DarkCyan,
            Color::Grey,
            Color::DarkGrey,
            Color::Red,
            Color::Green,
            Color::Yellow,
            Color::Blue,
            Color::Magenta,
            Color::Cyan,
            Color::White,
            Color::Rgb {
                r: 215,
                g: 119,
                b: 87,
            },
            Color::AnsiValue(174),
        ];
        for (index, case) in cases.as_array().unwrap().iter().enumerate() {
            chalk::set_stdout_level(case["level"].as_u64().unwrap() as u8);
            let channel = if case["type"] == "background" {
                ColorType::Background
            } else {
                ColorType::Foreground
            };
            assert_eq!(
                colorize(
                    case["text"].as_str().unwrap(),
                    Some(colors[(index / 6) % colors.len()]),
                    channel
                ),
                case["result"].as_str().unwrap(),
                "{case}"
            );
        }
    }

    #[test]
    fn color_helper_wraps_rgb_foreground_like_official_colorize() {
        chalk::set_stdout_level(3);
        let apply = color(Some(Color::Rgb { r: 1, g: 2, b: 3 }), ColorType::Foreground);

        assert_eq!(apply("hi"), "\u{1b}[38;2;1;2;3mhi\u{1b}[39m");
    }

    #[test]
    fn color_helper_supports_background_and_none_passthrough() {
        chalk::set_stdout_level(3);
        assert_eq!(
            colorize("hi", Some(Color::Blue), ColorType::Background),
            "\u{1b}[104mhi\u{1b}[49m"
        );
        assert_eq!(colorize("hi", None, ColorType::Foreground), "hi");
    }
}
