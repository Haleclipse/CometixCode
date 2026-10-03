//! Common date helpers.
//!
//! Maps to CC `constants/common.ts`.

use crate::utils::process_env::JsTruthy;

/// Maps to: CC `constants/common.ts` `getLocalISODate()`.
pub fn get_local_iso_date() -> String {
    if let Some(override_date) =
        crate::utils::process_env::var("CLAUDE_CODE_OVERRIDE_DATE").truthy()
    {
        return override_date;
    }

    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// Maps to: CC `constants/common.ts:28-33` `getLocalMonthYear()` —
/// `toLocaleString('en-US', { month: 'long', year: 'numeric' })` with the
/// same `CLAUDE_CODE_OVERRIDE_DATE` override; an unparseable override falls
/// back to now (CC would render "Invalid Date").
pub fn get_local_month_year() -> String {
    if let Some(override_date) =
        crate::utils::process_env::var("CLAUDE_CODE_OVERRIDE_DATE").truthy()
    {
        if let Ok(date) = chrono::NaiveDate::parse_from_str(&override_date, "%Y-%m-%d") {
            return date.format("%B %Y").to_string();
        }
    }
    chrono::Local::now().format("%B %Y").to_string()
}

/// Maps to: CC `constants/common.ts` memoized `getSessionStartDate`.
pub fn get_session_start_date() -> &'static str {
    static SESSION_START_DATE: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(get_local_iso_date);
    SESSION_START_DATE.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn get_local_iso_date_honors_official_override_env() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("CLAUDE_CODE_OVERRIDE_DATE", "2026-07-03");
        assert_eq!(get_local_iso_date(), "2026-07-03");
        crate::utils::process_env::remove("CLAUDE_CODE_OVERRIDE_DATE");
    }
}
