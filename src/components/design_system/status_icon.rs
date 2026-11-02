//! Maps to: CC `components/design-system/StatusIcon.tsx`.
//! Renders a single status glyph with the official status-to-color mapping.

use crate::constants::figures::MAIN_SYMBOLS;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum StatusIconStatus {
    Success,
    Error,
    Warning,
    Info,
    #[default]
    Pending,
    Loading,
}

pub(crate) fn status_icon_config(
    status: StatusIconStatus,
    theme: &Theme,
) -> (&'static str, Option<Color>) {
    match status {
        StatusIconStatus::Success => (MAIN_SYMBOLS.tick, Some(theme.success)),
        StatusIconStatus::Error => (MAIN_SYMBOLS.cross, Some(theme.error)),
        StatusIconStatus::Warning => (MAIN_SYMBOLS.warning, Some(theme.warning)),
        StatusIconStatus::Info => (MAIN_SYMBOLS.info, Some(theme.suggestion)),
        StatusIconStatus::Pending => (MAIN_SYMBOLS.circle, None),
        StatusIconStatus::Loading => ("…", None),
    }
}

#[derive(Default, Props)]
pub struct StatusIconProps {
    pub status: StatusIconStatus,
    pub with_space: bool,
}

#[component]
pub fn StatusIcon(props: &StatusIconProps, hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let (icon, color) = status_icon_config(props.status, &theme);

    element! {
        Text(
            content: format!("{}{}", icon, if props.with_space { " " } else { "" }),
            color: color,
            dim: color.is_none(),
            wrap: TextWrap::NoWrap,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    #[test]
    fn status_icon_matches_official_status_glyphs_and_colors() {
        let current_theme = *theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                StatusIcon(status: StatusIconStatus::Success, with_space: true)
            }
        }
        .render(Some(20));

        assert_eq!(canvas.to_string().trim_end(), MAIN_SYMBOLS.tick.to_string());
        assert_eq!(
            canvas
                .cell(0, 0)
                .and_then(|cell| cell.text_style())
                .and_then(|style| style.color),
            Some(current_theme.success)
        );
    }

    #[test]
    fn status_icon_dims_pending_and_loading() {
        let current_theme = *theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(current_theme)) {
                StatusIcon(status: StatusIconStatus::Loading)
            }
        }
        .render(Some(20));

        assert_eq!(canvas.to_string().trim_end(), "…");
        assert_eq!(
            canvas
                .resolved_text_style(0, 0)
                .expect("loading icon style")
                .weight,
            Weight::Light
        );
    }
}
