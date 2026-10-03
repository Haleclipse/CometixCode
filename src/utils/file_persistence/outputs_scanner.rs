//! Maps to: CC `utils/filePersistence/outputsScanner.ts`.

/// Maps to: CC `getEnvironmentKind:25-31`.
/// The source's two string literals are retained without accepting other truthy values.
pub fn get_environment_kind() -> Option<String> {
    crate::utils::process_env::var("CLAUDE_CODE_ENVIRONMENT_KIND")
        .filter(|kind| matches!(kind.as_str(), "byoc" | "anthropic_cloud"))
}

#[cfg(test)]
mod tests {
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn environment_kind_matches_official_exact_remote_values() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        for value in ["", "local", "BYOC", " byoc", "byoc", "anthropic_cloud"] {
            let _env = EnvVarGuard::set("CLAUDE_CODE_ENVIRONMENT_KIND", value);
            assert_eq!(
                super::get_environment_kind().as_deref(),
                match value {
                    "byoc" | "anthropic_cloud" => Some(value),
                    _ => None,
                }
            );
        }
    }
}
