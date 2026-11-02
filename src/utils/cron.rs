//! Minimal cron expression parsing and next-run calculation.
//!
//! Maps to: CC `utils/cron.ts`.
//!
//! Supports the standard 5-field cron subset:
//!   minute hour day-of-month month day-of-week
//!
//! Field syntax: wildcard, N, step (star-slash-N), range (N-M), list (N,M,...).
//! No L, W, ?, or name aliases. All times are interpreted in the process's
//! local timezone — "0 9 * * *" means 9am wherever the CLI is running.

use chrono::{Datelike, Duration, Local, TimeZone, Timelike};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CronFields {
    pub minute: Vec<u32>,
    pub hour: Vec<u32>,
    pub day_of_month: Vec<u32>,
    pub month: Vec<u32>,
    pub day_of_week: Vec<u32>,
}

#[derive(Clone, Copy)]
struct FieldRange {
    min: u32,
    max: u32,
}

const FIELD_RANGES: [FieldRange; 5] = [
    FieldRange { min: 0, max: 59 }, // minute
    FieldRange { min: 0, max: 23 }, // hour
    FieldRange { min: 1, max: 31 }, // dayOfMonth
    FieldRange { min: 1, max: 12 }, // month
    FieldRange { min: 0, max: 6 },  // dayOfWeek (0=Sunday; 7 accepted as Sunday alias)
];

const DAY_NAMES: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// Parse a single cron field into a sorted array of matching values.
/// Supports: wildcard, N, star-slash-N (step), N-M (range), and comma-lists.
/// Returns None if invalid.
fn expand_field(field: &str, range: FieldRange) -> Option<Vec<u32>> {
    let FieldRange { min, max } = range;
    let mut out = std::collections::BTreeSet::new();

    for part in field.split(',') {
        // wildcard or star-slash-N
        if let Some(rest) = part.strip_prefix('*') {
            let step = if rest.is_empty() {
                1
            } else {
                let step_str = rest.strip_prefix('/')?;
                let step = step_str.parse::<u32>().ok()?;
                if step < 1 {
                    return None;
                }
                step
            };
            let mut i = min;
            while i <= max {
                out.insert(i);
                i += step;
            }
            continue;
        }

        // N-M or N-M/S
        if let Some((lo_s, rest)) = part.split_once('-') {
            let (hi_s, step) = if let Some((hi_s, step_s)) = rest.split_once('/') {
                let step = step_s.parse::<u32>().ok()?;
                (hi_s, step)
            } else {
                (rest, 1u32)
            };
            let lo = lo_s.parse::<u32>().ok()?;
            let hi = hi_s.parse::<u32>().ok()?;
            // dayOfWeek: accept 7 as Sunday alias in ranges (e.g. 5-7 = Fri,Sat,Sun → [5,6,0])
            let is_dow = min == 0 && max == 6;
            let eff_max = if is_dow { 7 } else { max };
            if lo > hi || step < 1 || lo < min || hi > eff_max {
                return None;
            }
            let mut i = lo;
            while i <= hi {
                out.insert(if is_dow && i == 7 { 0 } else { i });
                i += step;
            }
            continue;
        }

        // plain N
        if part.chars().all(|c| c.is_ascii_digit()) {
            let mut n = part.parse::<u32>().ok()?;
            // dayOfWeek: accept 7 as Sunday alias → 0
            if min == 0 && max == 6 && n == 7 {
                n = 0;
            }
            if n < min || n > max {
                return None;
            }
            out.insert(n);
            continue;
        }

        return None;
    }

    if out.is_empty() {
        return None;
    }
    Some(out.into_iter().collect())
}

/// Parse a 5-field cron expression into expanded number arrays.
/// Returns None if invalid or unsupported syntax.
/// Maps to: CC `utils/cron.ts` `parseCronExpression`.
pub fn parse_cron_expression(expr: &str) -> Option<CronFields> {
    let parts: Vec<&str> = expr.split_whitespace().collect();
    if parts.len() != 5 {
        return None;
    }

    let mut expanded = Vec::with_capacity(5);
    for i in 0..5 {
        let result = expand_field(parts[i], FIELD_RANGES[i])?;
        expanded.push(result);
    }

    Some(CronFields {
        minute: expanded[0].clone(),
        hour: expanded[1].clone(),
        day_of_month: expanded[2].clone(),
        month: expanded[3].clone(),
        day_of_week: expanded[4].clone(),
    })
}

/// Compute the next DateTime strictly after `from` that matches the cron fields,
/// using the process's local timezone. Walks forward minute-by-minute. Bounded
/// at 366 days; returns None if no match.
///
/// Maps to: CC `utils/cron.ts` `computeNextCronRun`.
pub fn compute_next_cron_run(
    fields: &CronFields,
    from: chrono::DateTime<Local>,
) -> Option<chrono::DateTime<Local>> {
    let minute_set: std::collections::HashSet<u32> = fields.minute.iter().copied().collect();
    let hour_set: std::collections::HashSet<u32> = fields.hour.iter().copied().collect();
    let dom_set: std::collections::HashSet<u32> = fields.day_of_month.iter().copied().collect();
    let month_set: std::collections::HashSet<u32> = fields.month.iter().copied().collect();
    let dow_set: std::collections::HashSet<u32> = fields.day_of_week.iter().copied().collect();

    let dom_wild = fields.day_of_month.len() == 31;
    let dow_wild = fields.day_of_week.len() == 7;

    // Round up to the next whole minute (strictly after `from`)
    let mut t = from
        .with_second(0)
        .and_then(|dt| dt.with_nanosecond(0))
        .unwrap_or(from);
    t += Duration::minutes(1);

    let max_iter = 366 * 24 * 60;
    for _ in 0..max_iter {
        let month = t.month();
        if !month_set.contains(&month) {
            // Jump to start of next month
            let year = t.year() + if t.month() == 12 { 1 } else { 0 };
            let next_month = if t.month() == 12 { 1 } else { t.month() + 1 };
            t = Local
                .with_ymd_and_hms(year, next_month, 1, 0, 0, 0)
                .single()?;
            continue;
        }

        let dom = t.day();
        let dow = t.weekday().num_days_from_sunday();
        // When both dom/dow are constrained, either match is sufficient (OR semantics)
        let day_matches = if dom_wild && dow_wild {
            true
        } else if dom_wild {
            dow_set.contains(&dow)
        } else if dow_wild {
            dom_set.contains(&dom)
        } else {
            dom_set.contains(&dom) || dow_set.contains(&dow)
        };

        if !day_matches {
            t = (t + Duration::days(1))
                .with_hour(0)
                .and_then(|dt| dt.with_minute(0))
                .and_then(|dt| dt.with_second(0))
                .and_then(|dt| dt.with_nanosecond(0))
                .unwrap_or(t + Duration::days(1));
            continue;
        }

        if !hour_set.contains(&t.hour()) {
            t = (t + Duration::hours(1))
                .with_minute(0)
                .and_then(|dt| dt.with_second(0))
                .and_then(|dt| dt.with_nanosecond(0))
                .unwrap_or(t + Duration::hours(1));
            continue;
        }

        if !minute_set.contains(&t.minute()) {
            t += Duration::minutes(1);
            continue;
        }

        return Some(t);
    }

    None
}

fn format_hour_minute_ampm(hour: u32, minute: u32) -> String {
    let (h12, ampm) = match hour {
        0 => (12, "AM"),
        1..=11 => (hour, "AM"),
        12 => (12, "PM"),
        _ => (hour - 12, "PM"),
    };
    format!("{h12}:{minute:02} {ampm}")
}

fn format_local_time(minute: u32, hour: u32) -> String {
    // January 1 — no DST gap anywhere (same rationale as CC cron.ts).
    let _ = Local.with_ymd_and_hms(2000, 1, 1, hour, minute, 0).single();
    format_hour_minute_ampm(hour, minute)
}

fn format_utc_time_as_local(minute: u32, hour: u32) -> String {
    use chrono::Utc;
    let d = Utc
        .with_ymd_and_hms(2000, 1, 1, hour, minute, 0)
        .single()
        .expect("fixed utc time")
        .with_timezone(&Local);
    format!(
        "{} {}",
        format_hour_minute_ampm(d.hour(), d.minute()),
        d.format("%Z")
    )
}

/// Maps to: CC `utils/cron.ts` `cronToHuman`.
pub fn cron_to_human(cron: &str, utc: bool) -> String {
    let parts: Vec<&str> = cron.split_whitespace().collect();
    if parts.len() != 5 {
        return cron.to_string();
    }
    let (minute, hour, day_of_month, month, day_of_week) =
        (parts[0], parts[1], parts[2], parts[3], parts[4]);

    // Every N minutes: */N * * * *
    if let Some(n_str) = minute.strip_prefix("*/")
        && hour == "*"
        && day_of_month == "*"
        && month == "*"
        && day_of_week == "*"
        && let Ok(n) = n_str.parse::<u32>()
    {
        return if n == 1 {
            "Every minute".to_string()
        } else {
            format!("Every {n} minutes")
        };
    }

    // Every hour: M * * * *
    if minute.chars().all(|c| c.is_ascii_digit())
        && hour == "*"
        && day_of_month == "*"
        && month == "*"
        && day_of_week == "*"
        && let Ok(m) = minute.parse::<u32>()
    {
        return if m == 0 {
            "Every hour".to_string()
        } else {
            format!("Every hour at :{m:02}")
        };
    }

    // Every N hours: M */N * * *
    if let Some(n_str) = hour.strip_prefix("*/")
        && minute.chars().all(|c| c.is_ascii_digit())
        && day_of_month == "*"
        && month == "*"
        && day_of_week == "*"
        && let (Ok(n), Ok(m)) = (n_str.parse::<u32>(), minute.parse::<u32>())
    {
        let suffix = if m == 0 {
            String::new()
        } else {
            format!(" at :{m:02}")
        };
        return if n == 1 {
            format!("Every hour{suffix}")
        } else {
            format!("Every {n} hours{suffix}")
        };
    }

    if !minute.chars().all(|c| c.is_ascii_digit()) || !hour.chars().all(|c| c.is_ascii_digit()) {
        return cron.to_string();
    }
    let Ok(m) = minute.parse::<u32>() else {
        return cron.to_string();
    };
    let Ok(h) = hour.parse::<u32>() else {
        return cron.to_string();
    };
    let fmt_time = |minute: u32, hour: u32| {
        if utc {
            format_utc_time_as_local(minute, hour)
        } else {
            format_local_time(minute, hour)
        }
    };

    // Daily at specific time: M H * * *
    if day_of_month == "*" && month == "*" && day_of_week == "*" {
        return format!("Every day at {}", fmt_time(m, h));
    }

    // Specific day of week: M H * * D
    if day_of_month == "*"
        && month == "*"
        && day_of_week.len() == 1
        && day_of_week.chars().all(|c| c.is_ascii_digit())
    {
        let day_index = day_of_week.parse::<usize>().unwrap_or(0) % 7;
        let day_name = if utc {
            use chrono::Utc;
            let ref_now = Utc::now();
            let days_to_add =
                (day_index as i64 - ref_now.weekday().num_days_from_sunday() as i64 + 7) % 7;
            let target = ref_now + Duration::days(days_to_add);
            let target = target
                .with_hour(h)
                .and_then(|dt| dt.with_minute(m))
                .and_then(|dt| dt.with_second(0))
                .unwrap_or(target);
            DAY_NAMES[target
                .with_timezone(&Local)
                .weekday()
                .num_days_from_sunday() as usize]
        } else {
            DAY_NAMES[day_index]
        };
        return format!("Every {day_name} at {}", fmt_time(m, h));
    }

    // Weekdays: M H * * 1-5
    if day_of_month == "*" && month == "*" && day_of_week == "1-5" {
        return format!("Weekdays at {}", fmt_time(m, h));
    }

    cron.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_every_five_minutes() {
        let fields = parse_cron_expression("*/5 * * * *").expect("valid");
        assert_eq!(
            fields.minute,
            vec![0, 5, 10, 15, 20, 25, 30, 35, 40, 45, 50, 55]
        );
        assert_eq!(fields.hour.len(), 24);
        assert_eq!(fields.day_of_week.len(), 7);
    }

    #[test]
    fn parse_rejects_bad_field_count() {
        assert!(parse_cron_expression("* * *").is_none());
    }

    #[test]
    fn compute_next_is_strictly_after() {
        let fields = parse_cron_expression("*/5 * * * *").unwrap();
        let from = Local
            .with_ymd_and_hms(2026, 7, 14, 10, 0, 0)
            .single()
            .unwrap();
        let next = compute_next_cron_run(&fields, from).unwrap();
        assert_eq!(next.minute(), 5);
        assert!(next > from);
    }

    #[test]
    fn cron_to_human_common_patterns() {
        assert_eq!(cron_to_human("*/5 * * * *", false), "Every 5 minutes");
        assert_eq!(cron_to_human("0 * * * *", false), "Every hour");
        assert_eq!(cron_to_human("0 9 * * 1-5", false), "Weekdays at 9:00 AM");
    }
}
