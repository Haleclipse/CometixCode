//! Maps to: CC `components/Passes/Passes.tsx`.
//!
//! Guest passes dialog rendering. Official code fetches OAuth-backed referral
//! eligibility/redemptions and copies the referral link with OSC clipboard. In
//! this safe UI slice those network/clipboard side effects are represented by
//! an explicit `PassesViewState` snapshot and `on_copy_link` callback.

use crate::components::design_system::pane::Pane;
use crate::constants::figures::TEARDROP_ASTERISK;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

pub const V1_TERMS_URL: &str =
    "https://support.claude.com/en/articles/13456702-claude-code-guest-passes";
pub const DEFAULT_TERMS_URL: &str =
    "https://support.claude.com/en/articles/12875061-claude-code-guest-passes";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PassStatus {
    pub pass_number: usize,
    pub is_available: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReferrerRewardInfo {
    pub currency: String,
    pub amount_minor_units: i64,
}

/// Maps to: CC `services/api/referral.ts#formatCreditAmount`.
pub fn format_credit_amount(reward: &ReferrerRewardInfo) -> String {
    let symbol = match reward.currency.as_str() {
        "USD" => "$",
        "EUR" => "€",
        "GBP" => "£",
        "BRL" => "R$",
        "CAD" => "CA$",
        "AUD" => "A$",
        "NZD" => "NZ$",
        "SGD" => "S$",
        currency => return format!("{} {}", currency, format_amount(reward.amount_minor_units)),
    };
    format!("{symbol}{}", format_amount(reward.amount_minor_units))
}

fn format_amount(minor_units: i64) -> String {
    let whole = minor_units / 100;
    let cents = (minor_units.abs() % 100) as u8;
    if cents == 0 {
        whole.to_string()
    } else {
        format!("{whole}.{cents:02}")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum PassesViewState {
    #[default]
    Loading,
    Unavailable,
    Available {
        pass_statuses: Vec<PassStatus>,
        referral_link: Option<String>,
        referrer_reward: Option<ReferrerRewardInfo>,
    },
}

/// Maps to: CC `Passes.tsx` sorted passes calculation.
pub fn sorted_passes(pass_statuses: &[PassStatus]) -> Vec<PassStatus> {
    let mut passes = pass_statuses.to_vec();
    passes.sort_by_key(|pass| !pass.is_available);
    passes
}

/// Maps to: CC `Passes.tsx` `availableCount` calculation.
pub fn available_pass_count(pass_statuses: &[PassStatus]) -> usize {
    pass_statuses
        .iter()
        .filter(|pass| pass.is_available)
        .count()
}

#[derive(Default, Props)]
pub struct PassesProps<'a> {
    pub state: PassesViewState,
    pub exit_pending: bool,
    pub exit_key_name: Option<String>,
    pub on_done: HandlerMut<'a, String>,
    pub on_copy_link: HandlerMut<'a, String>,
}

/// Maps to: CC `Passes(...)` render branches.
#[component]
pub fn Passes<'a>(props: &mut PassesProps<'a>, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let mut pending_copy = hooks.use_state(|| Option::<String>::None);
    let referral_link_for_events = match &props.state {
        PassesViewState::Available { referral_link, .. } => referral_link.clone(),
        _ => None,
    };

    hooks.use_terminal_events({
        let mut pending_copy = pending_copy;
        let referral_link_for_events = referral_link_for_events.clone();
        move |event| {
            let TerminalEvent::Key(KeyEvent { code, kind, .. }) = event else {
                return;
            };
            if kind == KeyEventKind::Release {
                return;
            }
            match code {
                KeyCode::Enter => {
                    if let Some(link) = referral_link_for_events.clone() {
                        pending_copy.set(Some(link));
                    }
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                    // Official calls onDone("Guest passes dialog dismissed", { display: 'system' }).
                    pending_copy.set(Some(String::new()));
                }
                _ => {}
            }
        }
    });

    let pending = { pending_copy.read().clone() };
    if let Some(link_or_cancel) = pending {
        pending_copy.set(None);
        if link_or_cancel.is_empty() {
            (props.on_done)("Guest passes dialog dismissed".to_string());
        } else {
            (props.on_copy_link)(link_or_cancel);
        }
    }

    let footer = if props.exit_pending {
        format!(
            "Press {} again to exit",
            props.exit_key_name.as_deref().unwrap_or("Ctrl-C")
        )
    } else {
        match props.state {
            PassesViewState::Available {
                ref referral_link, ..
            } if referral_link.is_some() => "Enter to copy link · Esc to cancel".to_string(),
            _ => "Esc to cancel".to_string(),
        }
    };

    match &props.state {
        PassesViewState::Loading => element! {
            Pane {
                View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
                    Text(content: "Loading guest pass information…".to_string(), color: theme.inactive)
                    Text(content: footer, color: theme.inactive, italic: true)
                }
            }
        }
        .into_any(),
        PassesViewState::Unavailable => element! {
            Pane {
                View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
                    Text(content: "Guest passes are not currently available.".to_string())
                    Text(content: footer, color: theme.inactive, italic: true)
                }
            }
        }
        .into_any(),
        PassesViewState::Available {
            pass_statuses,
            referral_link,
            referrer_reward,
        } => {
            let available_count = available_pass_count(pass_statuses);
            let sorted = sorted_passes(pass_statuses);
            let terms_url = if referrer_reward.is_some() {
                V1_TERMS_URL
            } else {
                DEFAULT_TERMS_URL
            };
            let share_text = if let Some(reward) = referrer_reward {
                format!(
                    "Share a free week of nyan with friends. If they love it and subscribe, you'll get {} of extra usage to keep building.",
                    format_credit_amount(reward)
                )
            } else {
                "Share a free week of nyan with friends.".to_string()
            };
            let theme_value = *theme;

            element! {
                Pane {
                    View(flex_direction: FlexDirection::Column, row_gap: 1u32) {
                        Text(content: format!("Guest passes · {available_count} left"), color: theme.permission)
                        View(flex_direction: FlexDirection::Row, margin_left: 2u32) {
                            #(sorted.into_iter().take(3).map(|pass| render_ticket(pass, theme_value)))
                        }
                        #(referral_link.as_ref().map(|link| element! {
                            View(margin_left: 2u32) {
                                Text(content: link.clone(), wrap: TextWrap::NoWrap)
                            }
                        }))
                        View(flex_direction: FlexDirection::Column, margin_left: 2u32) {
                            Text(content: share_text, color: theme.inactive, wrap: TextWrap::Wrap)
                            Text(content: "Terms apply.".to_string(), color: theme.inactive, href: Some(terms_url.to_string()))
                        }
                        View {
                            Text(content: footer, color: theme.inactive, italic: true)
                        }
                    }
                }
            }
            .into_any()
        }
    }
}

fn render_ticket(pass: PassStatus, theme: Theme) -> AnyElement<'static> {
    if !pass.is_available {
        return element! {
            View(flex_direction: FlexDirection::Column, margin_right: 1u32) {
                Text(content: "┌─────────╱".to_string(), color: theme.inactive)
                Text(content: format!(" ) CC {TEARDROP_ASTERISK} ┊╱"), color: theme.inactive)
                Text(content: "└───────╱".to_string(), color: theme.inactive)
            }
        }
        .into_any();
    }

    element! {
        View(flex_direction: FlexDirection::Column, margin_right: 1u32) {
            Text(content: "┌──────────┐".to_string())
            View(flex_direction: FlexDirection::Row) {
                Text(content: " ) CC ".to_string())
                Text(content: TEARDROP_ASTERISK.to_string(), color: theme.claude)
                Text(content: " ┊ ( ".to_string())
            }
            Text(content: "└──────────┘".to_string())
        }
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    #[test]
    fn format_credit_amount_matches_referral_helper() {
        assert_eq!(
            format_credit_amount(&ReferrerRewardInfo {
                currency: "USD".to_string(),
                amount_minor_units: 5000,
            }),
            "$50"
        );
        assert_eq!(
            format_credit_amount(&ReferrerRewardInfo {
                currency: "EUR".to_string(),
                amount_minor_units: 1250,
            }),
            "€12.50"
        );
        assert_eq!(
            format_credit_amount(&ReferrerRewardInfo {
                currency: "JPY".to_string(),
                amount_minor_units: 300,
            }),
            "JPY 3"
        );
    }

    #[test]
    fn passes_available_renders_tickets_link_terms_and_footer() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                Passes(state: PassesViewState::Available {
                    pass_statuses: vec![
                        PassStatus { pass_number: 1, is_available: false },
                        PassStatus { pass_number: 2, is_available: true },
                        PassStatus { pass_number: 3, is_available: true },
                    ],
                    referral_link: Some("https://claude.ai/referral/example".to_string()),
                    referrer_reward: Some(ReferrerRewardInfo { currency: "USD".to_string(), amount_minor_units: 5000 }),
                })
            }
        }.render(Some(120)).to_string();

        assert!(text.contains("Guest passes · 2 left"), "canvas=\n{text}");
        assert!(
            text.contains("https://claude.ai/referral/example"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("you'll get $50 of extra usage"),
            "canvas=\n{text}"
        );
        assert!(text.contains("Terms apply."), "canvas=\n{text}");
        assert!(
            text.contains("Enter to copy link · Esc to cancel"),
            "canvas=\n{text}"
        );
        assert!(text.contains("┌──────────┐"), "canvas=\n{text}");
        assert!(text.contains("┌─────────╱"), "canvas=\n{text}");
    }

    #[test]
    fn passes_loading_and_unavailable_keep_official_cancel_copy() {
        let loading = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                Passes(state: PassesViewState::Loading)
            }
        }
        .render(Some(80))
        .to_string();
        assert!(
            loading.contains("Loading guest pass information…"),
            "canvas=\n{loading}"
        );
        assert!(loading.contains("Esc to cancel"), "canvas=\n{loading}");

        let unavailable = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                Passes(state: PassesViewState::Unavailable)
            }
        }
        .render(Some(80))
        .to_string();
        assert!(
            unavailable.contains("Guest passes are not currently available."),
            "canvas=\n{unavailable}"
        );
        assert!(
            unavailable.contains("Esc to cancel"),
            "canvas=\n{unavailable}"
        );
    }
}
