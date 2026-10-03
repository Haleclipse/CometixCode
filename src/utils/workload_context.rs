//! Process-scoped workload attribution for the headless CLI.
//!
//! Maps to CC `utils/workloadContext.ts`. CC uses `AsyncLocalStorage` so
//! detached work can carry a turn-local value. The public `--workload` option
//! is process-scoped for SDK daemon subprocesses; this owner preserves that
//! boundary without mutating environment variables.

use std::sync::{LazyLock, RwLock};

static PROCESS_WORKLOAD: LazyLock<RwLock<Option<String>>> = LazyLock::new(|| RwLock::new(None));

/// Set the process workload supplied by the headless CLI.
pub fn set_process_workload(workload: Option<String>) {
    *PROCESS_WORKLOAD
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = workload;
}

/// Maps to CC `getWorkload()` for the process-scoped print owner.
pub fn get_workload() -> Option<String> {
    PROCESS_WORKLOAD
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn process_workload_round_trips() {
        let _guard = TEST_ENV_LOCK.lock().unwrap();
        let previous = get_workload();
        set_process_workload(Some("cron".to_string()));
        assert_eq!(get_workload().as_deref(), Some("cron"));
        set_process_workload(previous);
    }
}
