//! Maps to: CC `components/LogoV2/CondensedLogo.tsx`.
//!
//! Official hooks read AppState/model/settings and increment upsell impression
//! counters in effects. Cometix keeps this as a render boundary with pure input
//! props; callers own state reads, config writes, and analytics.

use super::animated_clawd::AnimatedClawd;
use super::clawd::Clawd;
use super::guest_passes_upsell::GuestPassesUpsell;
use super::overage_credit_upsell::OverageCreditUpsell;
use crate::components::logo_v2::logo_v2::{
    LOGO_DISPLAY_NAME, LogoDisplayData, format_cwd_line, format_model_and_billing,
    truncate_to_width,
};
use crate::components::offscreen_freeze::OffscreenFreeze;
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct CondensedLogoProps {
    pub columns: usize,
    pub data: LogoDisplayData,
    pub show_guest_passes_upsell: bool,
    pub guest_passes_reward_text: Option<String>,
    pub show_overage_credit_upsell: bool,
    pub overage_credit_amount: Option<String>,
}

#[component]
pub fn CondensedLogo(props: &CondensedLogoProps) -> impl Into<AnyElement<'static>> {
    let text_width = props.columns.saturating_sub(15).max(20);
    let version_prefix = format!("{LOGO_DISPLAY_NAME} v");
    let truncated_version = truncate_to_width(
        &props.data.version,
        text_width
            .saturating_sub(crate::components::logo_v2::logo_v2::display_width(
                &version_prefix,
            ))
            .max(6),
    );

    let model_billing = format_model_and_billing(
        &props.data.model_display_name,
        &props.data.billing_type,
        text_width,
    );
    let cwd_line = format_cwd_line(&props.data, text_width);
    let use_animated_clawd = crate::utils::fullscreen::is_fullscreen_env_enabled();

    element! {
        OffscreenFreeze {
            View(flex_direction: FlexDirection::Row, column_gap: 2u32, align_items: AlignItems::CENTER) {
                #(if use_animated_clawd {
                    element! { AnimatedClawd }.into_any()
                } else {
                    element! { Clawd }.into_any()
                })
                View(flex_direction: FlexDirection::Column) {
                    View(flex_direction: FlexDirection::Row) {
                        Text(content: LOGO_DISPLAY_NAME.to_string(), bold: true, wrap: TextWrap::NoWrap)
                        Text(content: " ".to_string(), wrap: TextWrap::NoWrap)
                        Text(content: format!("v{truncated_version}"), dim: true, wrap: TextWrap::NoWrap)
                    }
                    #(if model_billing.should_split {
                        element! {
                            View(flex_direction: FlexDirection::Column) {
                                Text(content: model_billing.truncated_model.clone(), dim: true, wrap: TextWrap::NoWrap)
                                Text(content: model_billing.truncated_billing.clone(), dim: true, wrap: TextWrap::NoWrap)
                            }
                        }.into_any()
                    } else {
                        element! {
                            Text(content: format!("{} · {}", model_billing.truncated_model, model_billing.truncated_billing), dim: true, wrap: TextWrap::NoWrap)
                        }.into_any()
                    })
                    Text(content: cwd_line, dim: true, wrap: TextWrap::NoWrap)
                    #(if props.show_guest_passes_upsell {
                        Some(element! { GuestPassesUpsell(reward_text: props.guest_passes_reward_text.clone()) }.into_any())
                    } else {
                        None
                    })
                    #(if !props.show_guest_passes_upsell && props.show_overage_credit_upsell {
                        Some(element! {
                            OverageCreditUpsell(
                                amount: props.overage_credit_amount.clone(),
                                max_width: Some(text_width),
                                two_line: true,
                            )
                        }.into_any())
                    } else {
                        None
                    })
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    fn render_condensed(data: LogoDisplayData, guest: bool, overage: bool) -> String {
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                CondensedLogo(
                    columns: 80usize,
                    data: data,
                    show_guest_passes_upsell: guest,
                    guest_passes_reward_text: Some("$5".to_string()),
                    show_overage_credit_upsell: overage,
                    overage_credit_amount: Some("$10".to_string()),
                )
            }
        }
        .render(Some(100))
        .to_string()
    }

    #[test]
    fn condensed_logo_renders_official_identity_rows_and_cwd() {
        let text = render_condensed(LogoDisplayData::fixture(), false, false);
        assert!(text.contains(LOGO_DISPLAY_NAME), "canvas=\n{text}");
        assert!(text.contains("v1.2.3"), "canvas=\n{text}");
        assert!(
            text.contains("Default (recommended) · API Usage Billing"),
            "canvas=\n{text}"
        );
        assert!(text.contains("/code/claude"), "canvas=\n{text}");
    }

    #[test]
    fn condensed_logo_prefers_guest_passes_over_overage_credit_like_official() {
        let guest = render_condensed(LogoDisplayData::fixture(), true, true);
        assert!(
            guest.contains("Share Claude Code and earn $5"),
            "canvas=\n{guest}"
        );
        assert!(!guest.contains("$10 in extra usage"), "canvas=\n{guest}");

        let overage = render_condensed(LogoDisplayData::fixture(), false, true);
        assert!(overage.contains("$10 in extra usage"), "canvas=\n{overage}");
        assert!(
            overage.contains("On us. Works on third-party apps"),
            "canvas=\n{overage}"
        );
    }
}
