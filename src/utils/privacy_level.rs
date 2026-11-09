//! Privacy-level network gates.
//!
//! Maps to: CC `utils/privacyLevel.ts:1-52`. @nyan: there is no telemetry, so
//! CC's `no-telemetry` level (`DISABLE_TELEMETRY`) gates nothing; only the
//! essential-traffic level remains.

use crate::utils::process_env::JsTruthy;

pub fn is_essential_traffic_only() -> bool {
    get_essential_traffic_only_reason().is_some()
}

pub fn get_essential_traffic_only_reason() -> Option<&'static str> {
    crate::utils::process_env::var("NYAN_DISABLE_NONESSENTIAL_TRAFFIC")
        .truthy()
        .is_some()
        .then_some("NYAN_DISABLE_NONESSENTIAL_TRAFFIC")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn essential_traffic_only_follows_the_nyan_switch() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        {
            let _traffic = EnvVarGuard::unset("NYAN_DISABLE_NONESSENTIAL_TRAFFIC");
            assert!(!is_essential_traffic_only());
            assert_eq!(get_essential_traffic_only_reason(), None);
        }
        let _traffic = EnvVarGuard::set("NYAN_DISABLE_NONESSENTIAL_TRAFFIC", "1");
        assert!(is_essential_traffic_only());
        assert_eq!(
            get_essential_traffic_only_reason(),
            Some("NYAN_DISABLE_NONESSENTIAL_TRAFFIC")
        );
    }
}
