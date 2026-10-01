//! Maps to CC `tools/ScheduleCronTool/prompt.ts`.

pub const DEFAULT_MAX_AGE_DAYS: u64 = 7;
pub const CRON_CREATE_TOOL_NAME: &str = "CronCreate";
pub const CRON_DELETE_TOOL_NAME: &str = "CronDelete";
pub const CRON_LIST_TOOL_NAME: &str = "CronList";

/// Maps to: CC `isKairosCronEnabled()` GrowthBook gate.
/// Cometix reads the hardcoded feature-switch collection instead of GrowthBook.
pub fn is_kairos_cron_enabled() -> bool {
    !crate::utils::env_utils::is_env_truthy(
        crate::utils::process_env::var("CLAUDE_CODE_DISABLE_CRON").as_deref(),
    ) && crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::KairosCron,
    )
}

/// Maps to: CC `isDurableCronEnabled()` GrowthBook gate, read from the
/// hardcoded feature-switch collection.
pub fn is_durable_cron_enabled() -> bool {
    crate::utils::feature_flags::feature_enabled(
        crate::utils::feature_flags::FeatureFlag::DurableCron,
    )
}

pub fn build_cron_create_description(durable_enabled: bool) -> String {
    if durable_enabled {
        "Schedule a prompt to run at a future time — either recurring on a cron schedule, or once at a specific time. Pass durable: true to persist to .claude/scheduled_tasks.json; otherwise session-only."
            .to_string()
    } else {
        "Schedule a prompt to run at a future time within this Claude session — either recurring on a cron schedule, or once at a specific time."
            .to_string()
    }
}

/// Maps to: CC `ScheduleCronTool/prompt.ts:74-121` `buildCronCreatePrompt`.
pub fn build_cron_create_prompt(durable_enabled: bool) -> String {
    let durability_section = if durable_enabled {
        "## Durability\n\nBy default (durable: false) the job lives only in this Claude session — nothing is written to disk, and the job is gone when Claude exits. Pass durable: true to write to .claude/scheduled_tasks.json so the job survives restarts. Only use durable: true when the user explicitly asks for the task to persist (\"keep doing this every day\", \"set this up permanently\"). Most \"remind me in 5 minutes\" / \"check back in an hour\" requests should stay session-only."
    } else {
        "## Session-only\n\nJobs live only in this Claude session — nothing is written to disk, and the job is gone when Claude exits."
    };

    let durable_runtime_note = if durable_enabled {
        "Durable jobs persist to .claude/scheduled_tasks.json and survive session restarts — on next launch they resume automatically. One-shot durable tasks that were missed while the REPL was closed are surfaced for catch-up. Session-only jobs die with the process. "
    } else {
        ""
    };

    format!(
        "Schedule a prompt to be enqueued at a future time. Use for both recurring schedules and one-shot reminders.\n\nUses standard 5-field cron in the user's local timezone: minute hour day-of-month month day-of-week. \"0 9 * * *\" means 9am local — no timezone conversion needed.\n\n## One-shot tasks (recurring: false)\n\nFor \"remind me at X\" or \"at <time>, do Y\" requests — fire once then auto-delete.\nPin minute/hour/day-of-month/month to specific values:\n  \"remind me at 2:30pm today to check the deploy\" → cron: \"30 14 <today_dom> <today_month> *\", recurring: false\n  \"tomorrow morning, run the smoke test\" → cron: \"57 8 <tomorrow_dom> <tomorrow_month> *\", recurring: false\n\n## Recurring jobs (recurring: true, the default)\n\nFor \"every N minutes\" / \"every hour\" / \"weekdays at 9am\" requests:\n  \"*/5 * * * *\" (every 5 min), \"0 * * * *\" (hourly), \"0 9 * * 1-5\" (weekdays at 9am local)\n\n## Avoid the :00 and :30 minute marks when the task allows it\n\nEvery user who asks for \"9am\" gets `0 9`, and every user who asks for \"hourly\" gets `0 *` — which means requests from across the planet land on the API at the same instant. When the user's request is approximate, pick a minute that is NOT 0 or 30:\n  \"every morning around 9\" → \"57 8 * * *\" or \"3 9 * * *\" (not \"0 9 * * *\")\n  \"hourly\" → \"7 * * * *\" (not \"0 * * * *\")\n  \"in an hour or so, remind me to...\" → pick whatever minute you land on, don't round\n\nOnly use minute 0 or 30 when the user names that exact time and clearly means it (\"at 9:00 sharp\", \"at half past\", coordinating with a meeting). When in doubt, nudge a few minutes early or late — the user will not notice, and the fleet will.\n\n{durability_section}\n\n## Runtime behavior\n\nJobs only fire while the REPL is idle (not mid-query). {durable_runtime_note}The scheduler adds a small deterministic jitter on top of whatever you pick: recurring tasks fire up to 10% of their period late (max 15 min); one-shot tasks landing on :00 or :30 fire up to 90 s early. Picking an off-minute is still the bigger lever.\n\nRecurring tasks auto-expire after {DEFAULT_MAX_AGE_DAYS} days — they fire one final time, then are deleted. This bounds session lifetime. Tell the user about the {DEFAULT_MAX_AGE_DAYS}-day limit when scheduling recurring jobs.\n\nReturns a job ID you can pass to {CRON_DELETE_TOOL_NAME}."
    )
}

pub const CRON_DELETE_DESCRIPTION: &str = "Cancel a scheduled cron job by ID";

pub fn build_cron_delete_prompt(durable_enabled: bool) -> String {
    if durable_enabled {
        format!(
            "Cancel a cron job previously scheduled with {CRON_CREATE_TOOL_NAME}. Removes it from .claude/scheduled_tasks.json (durable jobs) or the in-memory session store (session-only jobs)."
        )
    } else {
        format!(
            "Cancel a cron job previously scheduled with {CRON_CREATE_TOOL_NAME}. Removes it from the in-memory session store."
        )
    }
}

pub const CRON_LIST_DESCRIPTION: &str = "List scheduled cron jobs";

pub fn build_cron_list_prompt(durable_enabled: bool) -> String {
    if durable_enabled {
        format!(
            "List all cron jobs scheduled via {CRON_CREATE_TOOL_NAME}, both durable (.claude/scheduled_tasks.json) and session-only."
        )
    } else {
        format!("List all cron jobs scheduled via {CRON_CREATE_TOOL_NAME} in this session.")
    }
}
