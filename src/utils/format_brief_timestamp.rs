//! Maps to: CC `utils/formatBriefTimestamp.ts` (complete file).

use crate::utils::process_env::JsTruthy;
use chrono::{DateTime, Datelike, Local};

fn locale_prefers_12_hour_clock() -> bool {
    let locale = crate::utils::process_env::var("LC_ALL")
        .truthy()
        .or_else(|| crate::utils::process_env::var("LC_TIME").truthy())
        .or_else(|| crate::utils::process_env::var("LANG").truthy())
        .unwrap_or_default()
        .replace('-', "_")
        .to_ascii_lowercase();
    locale.is_empty() || locale.starts_with("en_us")
}

fn time_text(date: DateTime<Local>, include_weekday: bool, include_date: bool) -> String {
    let clock = if locale_prefers_12_hour_clock() {
        date.format("%-I:%M %p").to_string()
    } else {
        date.format("%H:%M").to_string()
    };
    if include_date {
        format!(
            "{}, {} {}, {clock}",
            date.format("%A"),
            date.format("%b"),
            date.day()
        )
    } else if include_weekday {
        format!("{}, {clock}", date.format("%A"))
    } else {
        clock
    }
}

/// Maps to: CC `utils/formatBriefTimestamp.ts:18-49` `formatBriefTimestamp`.
pub fn format_brief_timestamp(iso: &str, now: DateTime<Local>) -> String {
    let Ok(parsed) = DateTime::parse_from_rfc3339(iso) else {
        return String::new();
    };
    let date = parsed.with_timezone(&Local);
    let days = now
        .date_naive()
        .signed_duration_since(date.date_naive())
        .num_days();
    if days == 0 {
        time_text(date, false, false)
    } else if (1..7).contains(&days) {
        time_text(date, true, false)
    } else {
        time_text(date, true, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn format_brief_timestamp_matches_official_age_branches() {
        let now = Local.with_ymd_and_hms(2026, 1, 8, 12, 0, 0).unwrap();
        assert!(format_brief_timestamp("invalid", now).is_empty());
        assert!(!format_brief_timestamp("2026-01-08T10:00:00Z", now).contains(','));
        assert!(format_brief_timestamp("2026-01-05T10:00:00Z", now).contains(','));
        assert!(
            format_brief_timestamp("2025-12-20T10:00:00Z", now)
                .matches(',')
                .count()
                >= 2
        );
    }
}
