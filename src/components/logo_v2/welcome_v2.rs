//! Maps to: CC `components/LogoV2/WelcomeV2.tsx`.
//!
//! This is the standalone onboarding/welcome art component used by official
//! onboarding and CLI utility paths. Runtime onboarding state stays outside
//! this component, matching the upstream `WelcomeV2` responsibility boundary.

use crate::constants::product;
use crate::utils::env;
use crate::utils::theme::ThemeName;
use iocraft::prelude::*;

const WELCOME_V2_WIDTH: usize = 58;
const WELCOME_MESSAGE: &str = "Welcome to nyan";

#[derive(Default, Props)]
pub struct WelcomeV2Props {
    /// Test/runtime override for the official theme-name branch. When omitted,
    /// nyan uses its current default dark theme until theme-name context is
    /// promoted alongside the existing `Theme` color context.
    pub theme_name: Option<ThemeName>,
    pub version: Option<String>,
    pub apple_terminal: Option<bool>,
}

#[component]
pub fn WelcomeV2(props: &WelcomeV2Props, hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    let theme_name = props.theme_name.unwrap_or(ThemeName::Dark);
    let version = props
        .version
        .clone()
        .unwrap_or_else(|| product::VERSION.to_string());
    let apple_terminal = props
        .apple_terminal
        .unwrap_or_else(|| env::get().terminal.as_deref() == Some("Apple_Terminal"));
    let rows = welcome_v2_body_rows(theme_name, apple_terminal);

    element! {
        View(flex_direction: FlexDirection::Column, width: WELCOME_V2_WIDTH as u32) {
            View(flex_direction: FlexDirection::Row) {
                Text(content: format!("{WELCOME_MESSAGE} "), color: theme.claude, wrap: TextWrap::NoWrap)
                Text(content: format!("v{version} "), color: theme.inactive, wrap: TextWrap::NoWrap)
            }
            #(rows.into_iter().map(|line| element! {
                Text(content: line, wrap: TextWrap::NoWrap)
            }))
        }
    }
}

fn welcome_v2_body_rows(theme_name: ThemeName, apple_terminal: bool) -> Vec<String> {
    let light = matches!(
        theme_name,
        ThemeName::Light | ThemeName::LightAnsi | ThemeName::LightDaltonized
    );

    match (apple_terminal, light) {
        (true, true) => apple_light_rows(),
        (true, false) => apple_dark_rows(),
        (false, true) => light_rows(),
        (false, false) => dark_rows(),
    }
}

fn horizontal_rule() -> String {
    "…".repeat(WELCOME_V2_WIDTH)
}

fn blank() -> String {
    " ".repeat(WELCOME_V2_WIDTH)
}

fn dark_rows() -> Vec<String> {
    vec![
        horizontal_rule(),
        blank(),
        "     *                                       █████▓▓░     ".to_string(),
        "                                 *         ███▓░     ░░   ".to_string(),
        "            ░░░░░░                        ███▓░           ".to_string(),
        "    ░░░   ░░░░░░░░░░                      ███▓░           ".to_string(),
        "   ░░░░░░░░░░░░░░░░░░░    *                ██▓░░      ▓   ".to_string(),
        "                                             ░▓▓███▓▓░    ".to_string(),
        " *                                 ░░░░                   ".to_string(),
        "                                 ░░░░░░░░                 ".to_string(),
        "                               ░░░░░░░░░░░░░░░░           ".to_string(),
        "       █████████                                        * ".to_string(),
        "      ██▄█████▄██                        *                ".to_string(),
        "       █████████      *                                   ".to_string(),
        "…………………█ █   █ █………………………………………………………………………………".to_string(),
    ]
}

fn light_rows() -> Vec<String> {
    vec![
        horizontal_rule(),
        blank(),
        blank(),
        blank(),
        "            ░░░░░░                                        ".to_string(),
        "    ░░░   ░░░░░░░░░░                                      ".to_string(),
        "   ░░░░░░░░░░░░░░░░░░░                                    ".to_string(),
        blank(),
        "                           ░░░░                     ██    ".to_string(),
        "                         ░░░░░░░░░░               ██▒▒██  ".to_string(),
        "                                            ▒▒      ██   ▒".to_string(),
        "       █████████                          ▒▒░░▒▒      ▒ ▒▒".to_string(),
        "      ██▄█████▄██                           ▒▒         ▒▒ ".to_string(),
        "       █████████                           ░          ▒   ".to_string(),
        "…………………█ █   █ █…………………………………………░…………………………▒…………".to_string(),
    ]
}

fn apple_dark_rows() -> Vec<String> {
    vec![
        horizontal_rule(),
        blank(),
        "     *                                       █████▓▓░     ".to_string(),
        "                                 *         ███▓░     ░░   ".to_string(),
        "            ░░░░░░                        ███▓░           ".to_string(),
        "    ░░░   ░░░░░░░░░░                      ███▓░           ".to_string(),
        "   ░░░░░░░░░░░░░░░░░░░    *                ██▓░░      ▓   ".to_string(),
        "                                             ░▓▓███▓▓░    ".to_string(),
        " *                                 ░░░░                   ".to_string(),
        "                                 ░░░░░░░░                 ".to_string(),
        "                               ░░░░░░░░░░░░░░░░           ".to_string(),
        "                                                      * ".to_string(),
        "        ▗ ▗     ▖ ▖                       *                ".to_string(),
        "        █████████      *                                   ".to_string(),
        "…………………█ █   █ █………………………………………………………………………………".to_string(),
    ]
}

fn apple_light_rows() -> Vec<String> {
    vec![
        horizontal_rule(),
        blank(),
        blank(),
        blank(),
        "            ░░░░░░                                        ".to_string(),
        "    ░░░   ░░░░░░░░░░                                      ".to_string(),
        "   ░░░░░░░░░░░░░░░░░░░                                    ".to_string(),
        blank(),
        "                           ░░░░                     ██    ".to_string(),
        "                         ░░░░░░░░░░               ██▒▒██  ".to_string(),
        "                                            ▒▒      ██   ▒".to_string(),
        "                                          ▒▒░░▒▒      ▒ ▒▒".to_string(),
        "      ▗ ▗     ▖ ▖                           ▒▒         ▒▒ ".to_string(),
        "       █████████                           ░          ▒   ".to_string(),
        "…………………█ █   █ █…………………………………………░…………………………▒…………".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    fn render_welcome(theme_name: ThemeName, apple_terminal: bool) -> String {
        let canvas = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                WelcomeV2(
                    theme_name: Some(theme_name),
                    version: Some("1.2.3".to_string()),
                    apple_terminal: Some(apple_terminal),
                )
            }
        }
        .render(Some(WELCOME_V2_WIDTH));
        canvas.to_string()
    }

    #[test]
    fn welcome_v2_dark_branch_matches_official_ascii_shape() {
        let text = render_welcome(ThemeName::Dark, false);
        assert!(text.contains("Welcome to nyan v1.2.3"), "canvas=\n{text}");
        assert!(text.contains("█████▓▓░"), "canvas=\n{text}");
        assert!(text.contains("██▄█████▄██"), "canvas=\n{text}");
        assert!(text.contains("█ █   █ █"), "canvas=\n{text}");
        assert!(!text.contains("Cometix"), "canvas=\n{text}");
    }

    #[test]
    fn welcome_v2_light_branch_matches_official_light_scene() {
        let text = render_welcome(ThemeName::Light, false);
        assert!(text.contains("██▒▒██"), "canvas=\n{text}");
        assert!(text.contains("▒▒░░▒▒"), "canvas=\n{text}");
        assert!(text.contains("██▄█████▄██"), "canvas=\n{text}");
    }

    #[test]
    fn welcome_v2_apple_terminal_branch_uses_block_clawd() {
        let text = render_welcome(ThemeName::Dark, true);
        assert!(text.contains("▗ ▗     ▖ ▖"), "canvas=\n{text}");
        assert!(!text.contains("██▄█████▄██"), "canvas=\n{text}");
    }

    #[test]
    fn welcome_v2_body_branch_selector_matches_official_theme_terminal_cases() {
        assert!(
            welcome_v2_body_rows(ThemeName::LightAnsi, false)
                .iter()
                .any(|line| line.contains("██▒▒██"))
        );
        assert!(
            welcome_v2_body_rows(ThemeName::DarkDaltonized, true)
                .iter()
                .any(|line| line.contains("▗ ▗     ▖ ▖"))
        );
    }
}
