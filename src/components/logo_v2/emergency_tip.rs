//! Maps to: CC `components/LogoV2/EmergencyTip.tsx`.
//!
//! The official component reads a cached GrowthBook config value and records
//! the shown tip in global config. nyan has no GrowthBook client and keeps this
//! component pure: callers provide the tip and last-shown value, and no config
//! writes occur inside the render boundary.

use iocraft::prelude::*;

pub const EMERGENCY_TIP_CONFIG_NAME: &str = "tengu-top-of-feed-tip";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EmergencyTipColor {
    #[default]
    Dim,
    Warning,
    Error,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TipOfFeed {
    pub tip: String,
    pub color: EmergencyTipColor,
}

#[derive(Default, Props)]
pub struct EmergencyTipProps {
    pub tip: TipOfFeed,
    pub last_shown_tip: Option<String>,
}

#[component]
pub fn EmergencyTip(props: &EmergencyTipProps, hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    let tip = tip_of_feed_to_show(&props.tip, props.last_shown_tip.as_deref());

    element! {
        Fragment {
            #(tip.map(|tip| {
                let color = match tip.color {
                    EmergencyTipColor::Dim => theme.inactive,
                    EmergencyTipColor::Warning => theme.warning,
                    EmergencyTipColor::Error => theme.error,
                };
                element! {
                    View(padding_left: 2u32, flex_direction: FlexDirection::Column) {
                        Text(content: tip.tip, color: color, wrap: TextWrap::NoWrap)
                    }
                }
            }))
        }
    }
}

pub fn tip_of_feed_to_show(tip: &TipOfFeed, last_shown_tip: Option<&str>) -> Option<TipOfFeed> {
    if tip.tip.is_empty() {
        return None;
    }
    if last_shown_tip == Some(tip.tip.as_str()) {
        return None;
    }
    Some(tip.clone())
}

/// Maps to: CC `getDynamicConfig_CACHED_MAY_BE_STALE<TipOfFeed>(CONFIG_NAME, DEFAULT_TIP)`
/// for the specific `tengu-top-of-feed-tip` dynamic config.
///
/// This is intentionally a typed LogoV2 helper: it only parses an
/// already-present value and never refreshes, reads arbitrary keys, or writes
/// config.
pub fn tip_of_feed_from_value(value: Option<&serde_json::Value>) -> TipOfFeed {
    let Some(value) = value else {
        return TipOfFeed::default();
    };
    let tip = value
        .get("tip")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
        .to_string();
    if tip.is_empty() {
        return TipOfFeed::default();
    }
    let color = match value
        .get("color")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("dim")
    {
        "warning" => EmergencyTipColor::Warning,
        "error" => EmergencyTipColor::Error,
        _ => EmergencyTipColor::Dim,
    };
    TipOfFeed { tip, color }
}

/// Maps to: CC `EmergencyTip.tsx` `getTipOfFeed()`.
///
/// nyan resolves the `tengu-top-of-feed-tip` payload from the
/// source-controlled switch table instead of GrowthBook.
pub fn get_tip_of_feed() -> TipOfFeed {
    let value = crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::EmergencyTip,
    )
    .then(|| serde_json::Value::Object(serde_json::Map::new()));
    tip_of_feed_from_value(value.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    fn render_tip(tip: TipOfFeed, last_shown_tip: Option<&str>) -> String {
        let canvas = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                EmergencyTip(tip: tip, last_shown_tip: last_shown_tip.map(str::to_string))
            }
        }
        .render(Some(80));
        canvas.to_string()
    }

    #[test]
    fn emergency_tip_visibility_matches_official_new_tip_gate() {
        let tip = TipOfFeed {
            tip: "Important service notice".to_string(),
            color: EmergencyTipColor::Warning,
        };

        assert!(tip_of_feed_to_show(&TipOfFeed::default(), None).is_none());
        assert!(tip_of_feed_to_show(&tip, Some("Important service notice")).is_none());
        assert_eq!(tip_of_feed_to_show(&tip, Some("older")).unwrap(), tip);
    }

    #[test]
    fn emergency_tip_renders_official_padding_and_text_when_visible() {
        let text = render_tip(
            TipOfFeed {
                tip: "Important service notice".to_string(),
                color: EmergencyTipColor::Error,
            },
            None,
        );

        assert!(
            text.contains("  Important service notice"),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn emergency_tip_renders_nothing_for_repeated_tip() {
        let text = render_tip(
            TipOfFeed {
                tip: "Already shown".to_string(),
                color: EmergencyTipColor::Dim,
            },
            Some("Already shown"),
        );

        assert!(text.trim().is_empty(), "canvas=\n{text}");
    }

    #[test]
    fn emergency_tip_typed_cache_parser_matches_official_shape() {
        let tip = tip_of_feed_from_value(Some(&serde_json::json!({
            "tip": "Scheduled maintenance soon",
            "color": "warning",
        })));
        assert_eq!(tip.tip, "Scheduled maintenance soon");
        assert_eq!(tip.color, EmergencyTipColor::Warning);

        let invalid = tip_of_feed_from_value(Some(&serde_json::json!({
            "tip": "",
            "color": "error",
        })));
        assert_eq!(invalid, TipOfFeed::default());
    }

    #[test]
    fn emergency_tip_is_the_official_empty_default() {
        assert_eq!(get_tip_of_feed(), TipOfFeed::default());
    }

    #[test]
    fn emergency_tip_keeps_official_config_key() {
        assert_eq!(EMERGENCY_TIP_CONFIG_NAME, "tengu-top-of-feed-tip");
    }
}
