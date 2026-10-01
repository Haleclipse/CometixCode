//! User-facing Claude.ai rate-limit messages.
//!
//! Maps to: CC `services/rateLimitMessages.ts:1-344`.

use chrono::{Datelike, Local, TimeZone, Timelike};

use crate::services::claude_ai_limits::{ClaudeAiLimits, QuotaStatus, RateLimitType};

pub const RATE_LIMIT_ERROR_PREFIXES: [&str; 5] = [
    "You've hit your",
    "You've used",
    "You're now using extra usage",
    "You're close to",
    "You're out of extra usage",
];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RateLimitUiContext {
    pub subscription_type: Option<String>,
    pub has_extra_usage_enabled: bool,
    pub has_billing_access: bool,
    pub overage_provisioning_allowed: bool,
    pub is_remote_mode: bool,
}

impl RateLimitUiContext {
    /// Captured outside retained rendering by the interactive launch owner.
    pub fn from_global_config(config: &crate::utils::config::GlobalConfig) -> Self {
        let subscription_type = crate::utils::auth::get_subscription_type();
        let account = config.oauth_account.as_ref();
        let org_role = account.and_then(|account| account.organization_role.as_deref());
        let has_billing_access = crate::services::mock_rate_limits::mock_billing_access_override()
            .unwrap_or_else(|| match subscription_type.as_deref() {
                Some("max" | "pro") => true,
                Some("team" | "enterprise") => org_role.is_some_and(|role| {
                    matches!(role, "admin" | "billing" | "owner" | "primary_owner")
                }),
                _ => false,
            });
        let billing_type = account.and_then(|account| account.billing_type.as_deref());
        let overage_provisioning_allowed = subscription_type.is_some()
            && matches!(
                billing_type,
                Some(
                    "stripe_subscription"
                        | "stripe_subscription_contracted"
                        | "apple_subscription"
                        | "google_play_subscription"
                )
            );
        Self {
            subscription_type,
            has_extra_usage_enabled: account
                .and_then(|account| account.has_extra_usage_enabled)
                .unwrap_or(false),
            has_billing_access,
            overage_provisioning_allowed,
            is_remote_mode: crate::utils::env_utils::is_env_truthy(
                crate::utils::process_env::var("CLAUDE_CODE_REMOTE").as_deref(),
            ),
        }
    }

    fn is_team_or_enterprise(&self) -> bool {
        matches!(
            self.subscription_type.as_deref(),
            Some("team" | "enterprise")
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateLimitSeverity {
    Error,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RateLimitMessage {
    pub message: String,
    pub severity: RateLimitSeverity,
}

pub fn is_rate_limit_error_message(text: &str) -> bool {
    RATE_LIMIT_ERROR_PREFIXES
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

fn local_timezone_name() -> String {
    Local::now().format("%Z").to_string()
}

fn format_reset_time(timestamp: f64) -> Option<String> {
    if timestamp == 0.0 || !timestamp.is_finite() {
        return None;
    }
    let date = Local.timestamp_opt(timestamp as i64, 0).single()?;
    let now = Local::now();
    let more_than_day = timestamp - now.timestamp() as f64 > 24.0 * 60.0 * 60.0;
    let mut rendered = if more_than_day {
        let include_year = date.year() != now.year();
        match (include_year, date.minute() == 0) {
            (true, true) => date.format("%b %-d, %Y, %-I%P").to_string(),
            (true, false) => date.format("%b %-d, %Y, %-I:%M%P").to_string(),
            (false, true) => date.format("%b %-d, %-I%P").to_string(),
            (false, false) => date.format("%b %-d, %-I:%M%P").to_string(),
        }
    } else if date.minute() == 0 {
        date.format("%-I%P").to_string()
    } else {
        date.format("%-I:%M%P").to_string()
    };
    rendered.push_str(&format!(" ({})", local_timezone_name()));
    Some(rendered)
}

fn limit_name(
    rate_limit_type: Option<RateLimitType>,
    context: &RateLimitUiContext,
) -> &'static str {
    match rate_limit_type {
        Some(RateLimitType::FiveHour) => "session limit",
        Some(RateLimitType::SevenDay) => "weekly limit",
        Some(RateLimitType::SevenDayOpus) => "Opus limit",
        Some(RateLimitType::SevenDaySonnet)
            if matches!(
                context.subscription_type.as_deref(),
                Some("pro" | "enterprise")
            ) =>
        {
            "weekly limit"
        }
        Some(RateLimitType::SevenDaySonnet) => "Sonnet limit",
        Some(RateLimitType::Overage) => "usage limit",
        None => "usage limit",
    }
}

fn warning_upsell(
    rate_limit_type: Option<RateLimitType>,
    context: &RateLimitUiContext,
) -> Option<&'static str> {
    match rate_limit_type {
        Some(RateLimitType::FiveHour) => {
            if context.is_team_or_enterprise() {
                (!context.has_extra_usage_enabled && context.overage_provisioning_allowed)
                    .then_some("/extra-usage to request more")
            } else if matches!(context.subscription_type.as_deref(), Some("pro" | "max")) {
                Some("/upgrade to keep using Claude Code")
            } else {
                None
            }
        }
        Some(RateLimitType::Overage) if context.is_team_or_enterprise() => {
            (!context.has_extra_usage_enabled && context.overage_provisioning_allowed)
                .then_some("/extra-usage to request more")
        }
        _ => None,
    }
}

fn early_warning_text(limits: &ClaudeAiLimits, context: &RateLimitUiContext) -> Option<String> {
    let rate_limit_type = limits.rate_limit_type?;
    let mut name = match rate_limit_type {
        RateLimitType::FiveHour => "session limit",
        RateLimitType::SevenDay => "weekly limit",
        RateLimitType::SevenDayOpus => "Opus limit",
        // Unlike reached/overage-transition copy, CC always calls an early
        // seven-day Sonnet warning the Sonnet limit.
        RateLimitType::SevenDaySonnet => "Sonnet limit",
        RateLimitType::Overage => "extra usage",
    }
    .to_string();
    let used = limits
        .utilization
        .filter(|usage| *usage > 0.0)
        .map(|usage| (usage * 100.0).floor() as u64);
    let reset = limits.resets_at.and_then(format_reset_time);
    let upsell = warning_upsell(Some(rate_limit_type), context);
    let base = match (used, reset) {
        (Some(used), Some(reset)) => {
            format!("You've used {used}% of your {name} · resets {reset}")
        }
        (Some(used), None) => format!("You've used {used}% of your {name}"),
        (None, Some(reset)) => {
            if rate_limit_type == RateLimitType::Overage {
                name.push_str(" limit");
            }
            format!("Approaching {name} · resets {reset}")
        }
        (None, None) => {
            if rate_limit_type == RateLimitType::Overage {
                name.push_str(" limit");
            }
            format!("Approaching {name}")
        }
    };
    Some(match upsell {
        Some(upsell) => format!("{base} · {upsell}"),
        None => base,
    })
}

fn format_limit_reached_text(limit: &str, reset: &str) -> String {
    let base = format!("You've hit your {limit}{reset}");
    if crate::utils::build_profile::has_internal_capability(
        crate::utils::build_profile::InternalCapability::Api,
    ) {
        format!(
            "{base}. If you have feedback about this limit, post in #briarpatch-cc. You can reset your limits with /reset-limits"
        )
    } else {
        base
    }
}

fn limit_reached_text(limits: &ClaudeAiLimits, context: &RateLimitUiContext) -> String {
    let reset = limits
        .resets_at
        .and_then(format_reset_time)
        .map(|reset| format!(" · resets {reset}"))
        .unwrap_or_default();
    if limits.overage_status == Some(QuotaStatus::Rejected) {
        let overage_reset = limits.overage_resets_at.and_then(format_reset_time);
        let earliest = match (limits.resets_at, limits.overage_resets_at) {
            (Some(standard), Some(overage)) if standard < overage => limits.resets_at,
            (Some(_), Some(_)) => limits.overage_resets_at,
            (Some(_), None) => limits.resets_at,
            (None, Some(_)) => limits.overage_resets_at,
            (None, None) => None,
        }
        .and_then(format_reset_time)
        .or(overage_reset)
        .map(|reset| format!(" · resets {reset}"))
        .unwrap_or_default();
        if limits.overage_disabled_reason.as_deref() == Some("out_of_credits") {
            return format!("You're out of extra usage{earliest}");
        }
        return format_limit_reached_text("limit", &earliest);
    }
    format_limit_reached_text(limit_name(limits.rate_limit_type, context), &reset)
}

pub fn get_rate_limit_message(
    limits: &ClaudeAiLimits,
    context: &RateLimitUiContext,
) -> Option<RateLimitMessage> {
    if limits.is_using_overage {
        return (limits.overage_status == Some(QuotaStatus::AllowedWarning)).then(|| {
            RateLimitMessage {
                message: "You're close to your extra usage spending limit".to_string(),
                severity: RateLimitSeverity::Warning,
            }
        });
    }
    if limits.status == QuotaStatus::Rejected {
        return Some(RateLimitMessage {
            message: limit_reached_text(limits, context),
            severity: RateLimitSeverity::Error,
        });
    }
    if limits.status == QuotaStatus::AllowedWarning {
        if limits
            .utilization
            .is_some_and(|utilization| utilization < 0.7)
        {
            return None;
        }
        if context.is_team_or_enterprise()
            && context.has_extra_usage_enabled
            && !context.has_billing_access
        {
            return None;
        }
        return early_warning_text(limits, context).map(|message| RateLimitMessage {
            message,
            severity: RateLimitSeverity::Warning,
        });
    }
    None
}

pub fn get_rate_limit_error_message(
    limits: &ClaudeAiLimits,
    context: &RateLimitUiContext,
) -> Option<String> {
    get_rate_limit_message(limits, context).and_then(|message| {
        (message.severity == RateLimitSeverity::Error).then_some(message.message)
    })
}

pub fn get_rate_limit_warning(
    limits: &ClaudeAiLimits,
    context: &RateLimitUiContext,
) -> Option<String> {
    get_rate_limit_message(limits, context).and_then(|message| {
        (message.severity == RateLimitSeverity::Warning).then_some(message.message)
    })
}

pub fn get_using_overage_text(limits: &ClaudeAiLimits, context: &RateLimitUiContext) -> String {
    let name = limit_name(limits.rate_limit_type, context);
    if name == "usage limit" || name == "extra usage" {
        return "Now using extra usage".to_string();
    }
    let reset = limits
        .resets_at
        .and_then(format_reset_time)
        .map(|reset| format!(" · Your {name} resets {reset}"))
        .unwrap_or_default();
    format!("You're now using extra usage{reset}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warning_threshold_and_overage_messages_match_official() {
        let context = RateLimitUiContext {
            subscription_type: Some("max".to_string()),
            has_billing_access: true,
            ..Default::default()
        };
        let mut limits = ClaudeAiLimits {
            status: QuotaStatus::AllowedWarning,
            rate_limit_type: Some(RateLimitType::SevenDay),
            utilization: Some(0.69),
            ..Default::default()
        };
        assert!(get_rate_limit_warning(&limits, &context).is_none());
        limits.utilization = Some(0.8);
        assert_eq!(
            get_rate_limit_warning(&limits, &context).as_deref(),
            Some("You've used 80% of your weekly limit")
        );
        limits.status = QuotaStatus::Rejected;
        limits.is_using_overage = true;
        limits.overage_status = Some(QuotaStatus::AllowedWarning);
        assert_eq!(
            get_rate_limit_warning(&limits, &context).as_deref(),
            Some("You're close to your extra usage spending limit")
        );
    }

    #[test]
    fn sonnet_warning_and_overage_rejection_use_context_specific_names() {
        let context = RateLimitUiContext {
            subscription_type: Some("pro".to_string()),
            has_billing_access: true,
            ..Default::default()
        };
        let warning = ClaudeAiLimits {
            status: QuotaStatus::AllowedWarning,
            rate_limit_type: Some(RateLimitType::SevenDaySonnet),
            utilization: Some(0.8),
            ..Default::default()
        };
        assert_eq!(
            get_rate_limit_warning(&warning, &context).as_deref(),
            Some("You've used 80% of your Sonnet limit")
        );
        let rejected = ClaudeAiLimits {
            status: QuotaStatus::Rejected,
            rate_limit_type: Some(RateLimitType::Overage),
            ..Default::default()
        };
        assert!(
            get_rate_limit_error_message(&rejected, &context)
                .is_some_and(|message| message.starts_with("You've hit your usage limit"))
        );
    }

    #[test]
    fn rate_limit_prefix_detection_matches_official() {
        assert!(is_rate_limit_error_message("You've hit your weekly limit"));
        assert!(is_rate_limit_error_message("You're out of extra usage"));
        assert!(!is_rate_limit_error_message("API Error: 429"));
    }
}
