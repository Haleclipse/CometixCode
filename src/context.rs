//! Maps to CC `src/context.ts` — session/CLAUDE.md/git context assembly.
//!
//! Doubles as the module root for CC's `src/context/` directory (Rust 2018
//! `context.rs` + `context/`), which is a DIFFERENT set of source files: the
//! React context providers below. Nothing from `context/` belongs in this
//! file's own body.

pub mod modal_context;
pub mod notifications;
pub mod overlay_context;

use std::collections::BTreeMap;
use std::sync::{LazyLock, RwLock};

const MAX_STATUS_CHARS: usize = 2000;

static SYSTEM_PROMPT_INJECTION: LazyLock<RwLock<Option<String>>> =
    LazyLock::new(|| RwLock::new(None));
/// Maps to CC `context.ts:113-146` / `:151-190` lodash memoized context
/// owners. `None` means the session cache has not been populated yet.
static USER_CONTEXT_CACHE: LazyLock<RwLock<Option<BTreeMap<String, String>>>> =
    LazyLock::new(|| RwLock::new(None));
static SYSTEM_CONTEXT_CACHE: LazyLock<RwLock<Option<BTreeMap<String, String>>>> =
    LazyLock::new(|| RwLock::new(None));

/// Maps to CC `context.ts` `getSystemPromptInjection()`.
pub fn get_system_prompt_injection() -> Option<String> {
    SYSTEM_PROMPT_INJECTION
        .read()
        .ok()
        .and_then(|value| value.clone())
}

/// Maps to CC `context.ts` `setSystemPromptInjection(...)`.
pub fn set_system_prompt_injection(value: Option<String>) {
    if let Ok(mut slot) = SYSTEM_PROMPT_INJECTION.write() {
        *slot = value;
    }
    // Maps to CC `context.ts:27-33`: changing the injection immediately
    // invalidates both memoized outer context layers.
    clear_user_context_cache();
    clear_system_context_cache();
}

/// Maps to CC `getUserContext.cache.clear?.()`.
pub fn clear_user_context_cache() {
    if let Ok(mut cache) = USER_CONTEXT_CACHE.write() {
        *cache = None;
    }
}

/// Maps to CC `getSystemContext.cache.clear?.()`.
pub fn clear_system_context_cache() {
    if let Ok(mut cache) = SYSTEM_CONTEXT_CACHE.write() {
        *cache = None;
    }
}

#[cfg(test)]
pub(crate) fn seed_context_caches_for_test() {
    USER_CONTEXT_CACHE
        .write()
        .unwrap()
        .replace(BTreeMap::from([(
            "seeded-user".to_string(),
            "value".to_string(),
        )]));
    SYSTEM_CONTEXT_CACHE
        .write()
        .unwrap()
        .replace(BTreeMap::from([(
            "seeded-system".to_string(),
            "value".to_string(),
        )]));
}

#[cfg(test)]
pub(crate) fn context_cache_presence_for_test() -> (bool, bool) {
    (
        USER_CONTEXT_CACHE
            .read()
            .map(|cache| cache.is_some())
            .unwrap_or(false),
        SYSTEM_CONTEXT_CACHE
            .read()
            .map(|cache| cache.is_some())
            .unwrap_or(false),
    )
}

/// Maps to: CC `context.ts` `getUserContext()`.
///
/// Official Claude Code memoizes this for a session and prepends it with
/// `utils/api.ts#prependUserContext(...)` immediately before `deps.callModel`.
/// This port keeps the same read-only data shape: `claudeMd` when memory files
/// are enabled, plus `currentDate` on every production query.
#[cfg(not(test))]
pub fn get_user_context() -> BTreeMap<String, String> {
    if let Ok(cache) = USER_CONTEXT_CACHE.read() {
        if let Some(context) = cache.as_ref() {
            return context.clone();
        }
    }
    let additional_dirs = crate::bootstrap::state::get_additional_directories_for_claude_md();
    let context = build_user_context(
        |key| crate::utils::process_env::var(key),
        || crate::utils::claudemd::build_claude_md_context(),
        crate::constants::common::get_local_iso_date,
        !additional_dirs.is_empty(),
    );
    // Maps to: CC `context.ts:176` — publish only after the canonical context
    // producer applies CLAUDE.md gates; the classifier must never load it again.
    crate::bootstrap::state::set_cached_claude_md_content(context.get("claudeMd").cloned());
    if let Ok(mut cache) = USER_CONTEXT_CACHE.write() {
        *cache = Some(context.clone());
    }
    context
}

/// Cargo tests are the Rust counterpart of CC's `NODE_ENV === 'test'` path in
/// `prependUserContext(...)`: do not read host CLAUDE.md files or perturb every
/// query request unless a focused unit test calls [`build_user_context`] directly.
#[cfg(test)]
pub fn get_user_context() -> BTreeMap<String, String> {
    BTreeMap::new()
}

/// Maps to: CC `context.ts` `getSystemContext()`.
#[cfg(not(test))]
pub fn get_system_context() -> BTreeMap<String, String> {
    if let Ok(cache) = SYSTEM_CONTEXT_CACHE.read() {
        if let Some(context) = cache.as_ref() {
            return context.clone();
        }
    }
    let settings = crate::utils::settings::get_initial_settings();
    let context = build_system_context(
        |key| crate::utils::process_env::var(key),
        settings.include_git_instructions,
        get_git_status,
    );
    if let Ok(mut cache) = SYSTEM_CONTEXT_CACHE.write() {
        *cache = Some(context.clone());
    }
    context
}

/// Keep cargo tests host-independent, mirroring the official test path that
/// avoids expensive git/context side effects.
#[cfg(test)]
pub fn get_system_context() -> BTreeMap<String, String> {
    BTreeMap::new()
}

fn build_user_context(
    get_env: impl Fn(&str) -> Option<String>,
    build_claude_md_context: impl Fn() -> String,
    get_local_iso_date: impl Fn() -> String,
    has_explicit_add_dir: bool,
) -> BTreeMap<String, String> {
    let mut context = BTreeMap::new();

    // Maps to CC `context.ts` `shouldDisableClaudeMd`: hard-disable wins;
    // bare/simple mode skips auto-discovery but still honors explicit add-dir.
    let should_disable_claude_md = crate::utils::env_utils::is_env_truthy(
        get_env("CLAUDE_CODE_DISABLE_CLAUDE_MDS").as_deref(),
    ) || (crate::utils::env_utils::is_env_truthy(
        get_env("CLAUDE_CODE_SIMPLE").as_deref(),
    ) && !has_explicit_add_dir);
    if !should_disable_claude_md {
        let claude_md = build_claude_md_context();
        // CC uses string truthiness for both context and classifier cache.
        if !claude_md.is_empty() {
            context.insert("claudeMd".to_string(), claude_md);
        }
    }

    context.insert(
        "currentDate".to_string(),
        format!("Today's date is {}.", get_local_iso_date()),
    );
    context
}

fn build_system_context(
    get_env: impl Fn(&str) -> Option<String>,
    include_git_instructions_setting: Option<bool>,
    get_git_status: impl Fn() -> Option<String>,
) -> BTreeMap<String, String> {
    build_system_context_with_cache_breaker(
        get_env,
        include_git_instructions_setting,
        get_git_status,
        crate::utils::feature_flags::feature_enabled(
            crate::utils::feature_flags::FeatureFlag::BreakCacheCommand,
        ),
        get_system_prompt_injection(),
    )
}

fn build_system_context_with_cache_breaker(
    get_env: impl Fn(&str) -> Option<String>,
    include_git_instructions_setting: Option<bool>,
    get_git_status: impl Fn() -> Option<String>,
    break_cache_command_enabled: bool,
    system_prompt_injection: Option<String>,
) -> BTreeMap<String, String> {
    let mut context = BTreeMap::new();

    // Maps to CC `context.ts`: skip git status in Claude Code Remote and when
    // `utils/gitSettings.ts#shouldIncludeGitInstructions()` returns false.
    if !crate::utils::env_utils::is_env_truthy(get_env("CLAUDE_CODE_REMOTE").as_deref())
        && should_include_git_instructions(&get_env, include_git_instructions_setting)
    {
        if let Some(git_status) = get_git_status() {
            context.insert("gitStatus".to_string(), git_status);
        }
    }

    // Maps to CC `context.ts` `feature('BREAK_CACHE_COMMAND')` gated
    // `cacheBreaker: [CACHE_BREAKER: ...]` system-context injection.
    if break_cache_command_enabled {
        if let Some(injection) = system_prompt_injection
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            context.insert(
                "cacheBreaker".to_string(),
                format!("[CACHE_BREAKER: {injection}]"),
            );
        }
    }

    context
}

fn get_git_status() -> Option<String> {
    // Maps to CC `context.ts` `getGitStatus()`.
    if run_git(["rev-parse", "--is-inside-work-tree"])? != "true" {
        return None;
    }

    let branch = run_git(["branch", "--show-current"]).unwrap_or_default();
    let main_branch = run_git(["rev-parse", "--abbrev-ref", "origin/HEAD"])
        .and_then(|value| value.strip_prefix("origin/").map(str::to_string))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "main".to_string());
    let status = truncate_git_status(
        run_git(["--no-optional-locks", "status", "--short"]).unwrap_or_default(),
    );
    let log = run_git(["--no-optional-locks", "log", "--oneline", "-n", "5"]).unwrap_or_default();
    let user_name = run_git(["config", "user.name"]).unwrap_or_default();

    let mut parts = vec![
        "This is the git status at the start of the conversation. Note that this status is a snapshot in time, and will not update during the conversation.".to_string(),
        format!("Current branch: {branch}"),
        format!("Main branch (you will usually use this for PRs): {main_branch}"),
    ];
    if !user_name.is_empty() {
        parts.push(format!("Git user: {user_name}"));
    }
    parts.push(format!(
        "Status:\n{}",
        if status.is_empty() {
            "(clean)"
        } else {
            &status
        }
    ));
    parts.push(format!("Recent commits:\n{log}"));
    Some(parts.join("\n\n"))
}

fn run_git<const N: usize>(args: [&str; N]) -> Option<String> {
    let mut command = std::process::Command::new(crate::utils::git::git_exe());
    // CC inherits process.env (execFileNoThrow); the carrier is its counterpart.
    crate::utils::subprocess_env::apply_process_env_std(&mut command);
    let output = command.args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn truncate_git_status(status: String) -> String {
    if status.len() <= MAX_STATUS_CHARS {
        return status;
    }
    let mut truncated = status.chars().take(MAX_STATUS_CHARS).collect::<String>();
    truncated.push_str("\n... (truncated because it exceeds 2k characters. If you need more information, run \"git status\" using BashTool)");
    truncated
}

/// Maps to: CC `utils/gitSettings.ts` `shouldIncludeGitInstructions()`.
fn should_include_git_instructions(
    get_env: &impl Fn(&str) -> Option<String>,
    include_git_instructions_setting: Option<bool>,
) -> bool {
    match get_env("CLAUDE_CODE_DISABLE_GIT_INSTRUCTIONS") {
        Some(value) if crate::utils::env_utils::is_env_truthy(Some(&value)) => false,
        Some(value) if crate::utils::env_utils::is_env_defined_falsy(Some(&value)) => true,
        _ => include_git_instructions_setting.unwrap_or(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_user_context_shape_matches_official_current_date_and_claude_md() {
        let context = build_user_context(
            |_| None,
            || "# Source: CLAUDE.md\n\nproject memory".to_string(),
            || "2026-07-03".to_string(),
            false,
        );

        assert_eq!(
            context.get("currentDate").map(String::as_str),
            Some("Today's date is 2026-07-03.")
        );
        assert_eq!(
            context.get("claudeMd").map(String::as_str),
            Some("# Source: CLAUDE.md\n\nproject memory")
        );
    }

    #[test]
    fn get_user_context_respects_official_claude_md_disable_gates() {
        for key in ["CLAUDE_CODE_DISABLE_CLAUDE_MDS", "CLAUDE_CODE_SIMPLE"] {
            let context = build_user_context(
                |requested| (requested == key).then(|| "1".to_string()),
                || "should not be read into context".to_string(),
                || "2026-07-03".to_string(),
                false,
            );

            assert_eq!(
                context.get("currentDate").map(String::as_str),
                Some("Today's date is 2026-07-03.")
            );
            assert!(!context.contains_key("claudeMd"));
        }
    }

    #[test]
    fn get_user_context_honors_explicit_add_dir_in_simple_mode_like_official() {
        let context = build_user_context(
            |requested| (requested == "CLAUDE_CODE_SIMPLE").then(|| "1".to_string()),
            || "# Source: /extra/CLAUDE.md\n\nexplicit memory".to_string(),
            || "2026-07-03".to_string(),
            true,
        );

        assert_eq!(
            context.get("claudeMd").map(String::as_str),
            Some("# Source: /extra/CLAUDE.md\n\nexplicit memory")
        );
    }

    #[test]
    fn get_system_context_includes_git_status_when_official_gate_allows() {
        let context = build_system_context(
            |_| None,
            None,
            || {
                Some(
                    "This is the git status at the start of the conversation.\n\nCurrent branch: main"
                        .to_string(),
                )
            },
        );

        assert!(
            context
                .get("gitStatus")
                .is_some_and(|value| value.contains("Current branch: main"))
        );
    }

    #[test]
    fn get_system_context_adds_cache_breaker_only_when_official_feature_enabled() {
        let disabled = build_system_context_with_cache_breaker(
            |_| None,
            Some(false),
            || Some("git status should be skipped".to_string()),
            false,
            Some("debug nonce".to_string()),
        );
        assert!(!disabled.contains_key("cacheBreaker"));
        assert!(!disabled.contains_key("gitStatus"));

        let enabled = build_system_context_with_cache_breaker(
            |_| None,
            Some(false),
            || Some("git status should be skipped".to_string()),
            true,
            Some("debug nonce".to_string()),
        );
        assert_eq!(
            enabled.get("cacheBreaker").map(String::as_str),
            Some("[CACHE_BREAKER: debug nonce]")
        );
    }

    #[test]
    fn system_prompt_injection_round_trips_like_official_ephemeral_state() {
        let previous = get_system_prompt_injection();
        set_system_prompt_injection(Some("nonce".to_string()));
        assert_eq!(get_system_prompt_injection().as_deref(), Some("nonce"));
        set_system_prompt_injection(None);
        assert_eq!(get_system_prompt_injection(), None);
        set_system_prompt_injection(previous);
    }

    #[test]
    fn get_system_context_skips_git_status_for_remote_or_disabled_git_instructions() {
        for key in ["CLAUDE_CODE_REMOTE", "CLAUDE_CODE_DISABLE_GIT_INSTRUCTIONS"] {
            let context = build_system_context(
                |requested| (requested == key).then(|| "1".to_string()),
                None,
                || Some("should not be inserted".to_string()),
            );

            assert!(!context.contains_key("gitStatus"));
        }
    }

    #[test]
    fn get_system_context_respects_settings_backed_git_instruction_gate() {
        let disabled_by_settings = build_system_context(
            |_| None,
            Some(false),
            || Some("should not be inserted".to_string()),
        );
        assert!(!disabled_by_settings.contains_key("gitStatus"));

        let env_falsy_forces_enabled_like_official = build_system_context(
            |requested| {
                (requested == "CLAUDE_CODE_DISABLE_GIT_INSTRUCTIONS").then(|| "0".to_string())
            },
            Some(false),
            || Some("setting was overridden by falsy env".to_string()),
        );
        assert_eq!(
            env_falsy_forces_enabled_like_official
                .get("gitStatus")
                .map(String::as_str),
            Some("setting was overridden by falsy env")
        );
    }

    #[test]
    fn truncate_git_status_matches_official_limit_copy() {
        let truncated = truncate_git_status("x".repeat(MAX_STATUS_CHARS + 1));
        assert!(truncated.starts_with(&"x".repeat(MAX_STATUS_CHARS)));
        assert!(truncated.contains("truncated because it exceeds 2k characters"));
        assert!(truncated.contains("git status"));
    }
}
