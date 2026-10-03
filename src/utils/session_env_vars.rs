//! Session-scoped child-process environment variables.
//!
//! Maps to: CC `utils/sessionEnvVars.ts:1-22`. The store is an `IndexMap`
//! because CC's `Map` iterates in insertion order. The order does not reach
//! the child: `std::process::Command` keeps its environment sorted, while
//! later keys still override earlier ones as in the bash provider's `for…of`.

use indexmap::IndexMap;
use std::sync::{LazyLock, RwLock};

static SESSION_ENV_VARS: LazyLock<RwLock<IndexMap<String, String>>> =
    LazyLock::new(|| RwLock::new(IndexMap::new()));

/// Maps to CC `getSessionEnvVars()`.
pub fn get_session_env_vars() -> IndexMap<String, String> {
    SESSION_ENV_VARS
        .read()
        .map(|vars| vars.clone())
        .unwrap_or_default()
}

/// Maps to CC `setSessionEnvVar(name, value)`: `Map.set` keeps an existing
/// key's position.
pub fn set_session_env_var(name: impl Into<String>, value: impl Into<String>) {
    if let Ok(mut vars) = SESSION_ENV_VARS.write() {
        vars.insert(name.into(), value.into());
    }
}

/// Maps to CC `deleteSessionEnvVar(name)`: `Map.delete` leaves the other
/// keys in order.
pub fn delete_session_env_var(name: &str) {
    if let Ok(mut vars) = SESSION_ENV_VARS.write() {
        vars.shift_remove(name);
    }
}

/// Maps to CC `clearSessionEnvVars()`.
pub fn clear_session_env_vars() {
    if let Ok(mut vars) = SESSION_ENV_VARS.write() {
        vars.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;

    #[test]
    fn session_env_var_mutations_match_official_map_contract() {
        struct Restore(IndexMap<String, String>);
        impl Drop for Restore {
            fn drop(&mut self) {
                if let Ok(mut vars) = SESSION_ENV_VARS.write() {
                    *vars = std::mem::take(&mut self.0);
                }
            }
        }

        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _restore = Restore(get_session_env_vars());
        clear_session_env_vars();
        set_session_env_var("B", "two");
        set_session_env_var("A", "one");
        set_session_env_var("C", "three");
        set_session_env_var("B", "again");
        assert_eq!(
            get_session_env_vars().keys().collect::<Vec<_>>(),
            ["B", "A", "C"]
        );
        assert_eq!(
            get_session_env_vars().get("B").map(String::as_str),
            Some("again")
        );
        // Deleting the first key: a swap-remove would move C in front of A.
        delete_session_env_var("B");
        assert_eq!(
            get_session_env_vars().keys().collect::<Vec<_>>(),
            ["A", "C"]
        );
        clear_session_env_vars();
        assert!(get_session_env_vars().is_empty());
    }
}
