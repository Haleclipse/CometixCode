//! Maps to: CC `components/Spinner/SpinnerGlyph.tsx`.

use super::utils::{interpolate_terminal_color, spinner_frame};
use crate::utils::theme::Theme;
use iocraft::prelude::*;

const REDUCED_MOTION_DOT: &str = "●";
const REDUCED_MOTION_CYCLE_MS: u64 = 2000;

#[derive(Default, Props)]
pub struct SpinnerGlyphProps {
    pub frame: usize,
    pub color: Option<Color>,
    pub reduced_motion: bool,
    pub time_ms: u64,
    pub stalled_intensity: f32,
}

/// Single spinner glyph cell. The normal path uses official frame characters;
/// reduced motion uses the official dot branch and the stalled path fades
/// toward the active theme's error color.
#[component]
pub fn SpinnerGlyph(props: &SpinnerGlyphProps, hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let base_color = props.color.unwrap_or(theme.claude);
    let color = if props.reduced_motion {
        base_color
    } else {
        interpolate_terminal_color(
            base_color,
            theme.error,
            props.stalled_intensity.clamp(0.0, 1.0),
        )
    };
    let glyph = if props.reduced_motion {
        REDUCED_MOTION_DOT
    } else {
        spinner_frame(props.frame)
    };
    let dim = props.reduced_motion && (props.time_ms / (REDUCED_MOTION_CYCLE_MS / 2)) % 2 == 1;

    element! {
        View(width: 2u32, height: 1u32, flex_shrink: 0.0f32) {
            Text(content: glyph, color: color, dim: dim, wrap: TextWrap::NoWrap)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spinner_glyph_uses_official_reduced_motion_dot_without_frame_animation() {
        let canvas = element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                SpinnerGlyph(frame: 3usize, reduced_motion: true)
            }
        }
        .render(None);

        assert_eq!(canvas.to_string().trim_end(), REDUCED_MOTION_DOT);
    }
}
