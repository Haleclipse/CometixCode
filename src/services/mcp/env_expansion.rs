//! MCP environment-variable expansion helpers.
//!
//! Maps to: CC `services/mcp/envExpansion.ts`.

/// Maps to CC `envExpansion.ts#expandEnvVarsInString`.
pub fn expand_env_vars_in_string(value: &str) -> (String, Vec<String>) {
    const PREFIX: &str = "${";
    let mut missing_vars = Vec::new();
    let mut out = String::with_capacity(value.len());
    let mut rest = value;

    while let Some(start) = rest.find(PREFIX) {
        out.push_str(&rest[..start]);
        let after_prefix = &rest[start + PREFIX.len()..];
        let Some(end) = after_prefix.find('}') else {
            out.push_str(&rest[start..]);
            return (out, missing_vars);
        };
        let var_content = &after_prefix[..end];
        let original = &rest[start..start + PREFIX.len() + end + 1];
        let (name, default_value) = var_content
            .split_once(":-")
            .map(|(name, default_value)| (name, Some(default_value)))
            .unwrap_or((var_content, None));
        if let Some(env_value) = crate::utils::process_env::var(name) {
            out.push_str(&env_value);
        } else if let Some(default_value) = default_value {
            out.push_str(default_value);
        } else {
            missing_vars.push(name.to_string());
            out.push_str(original);
        }
        rest = &after_prefix[end + 1..];
    }

    out.push_str(rest);
    (out, missing_vars)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn expand_env_vars_handles_values_defaults_and_missing_like_official() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_env::set("COMETIX_MCP_ENV_PRESENT", "value");
        crate::utils::process_env::remove("COMETIX_MCP_ENV_MISSING");
        let (expanded, missing) = expand_env_vars_in_string(
            "a=${COMETIX_MCP_ENV_PRESENT};b=${COMETIX_MCP_ENV_MISSING:-fallback};c=${COMETIX_MCP_ENV_MISSING}",
        );
        assert_eq!(expanded, "a=value;b=fallback;c=${COMETIX_MCP_ENV_MISSING}");
        assert_eq!(missing, vec!["COMETIX_MCP_ENV_MISSING".to_string()]);
        crate::utils::process_env::remove("COMETIX_MCP_ENV_PRESENT");
    }
}
