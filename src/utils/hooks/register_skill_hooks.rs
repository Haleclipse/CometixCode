//! Register skill frontmatter hooks as session hooks.
//! Maps to: CC `utils/hooks/registerSkillHooks.ts`.
//!
//! Official receives `setAppState` and mutates `AppState.sessionHooks`. Nyan
//! stores session hooks in `session_hooks.rs` until full AppState session hook
//! parity lands, but preserves the official event/matcher/skillRoot boundary.

use super::session_hooks::{add_session_hook_with_skill_root, remove_session_hook};
use crate::services::hooks::{HOOK_EVENTS, HookCommand, HookEvent, HooksConfig};

/// Result summary for `register_skill_hooks(...)`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RegisterSkillHooksResult {
    pub registered_count: usize,
    /// Hooks with `once: true` are registered and retain their flag. CC removes
    /// them via an `onHookSuccess` callback after execution; Nyan exposes the
    /// same removal helper below until the shared hook orchestrator wires
    /// success callbacks.
    pub one_shot_count: usize,
}

/// Maps to: CC `registerSkillHooks(...)`.
pub fn register_skill_hooks(
    session_id: &str,
    hooks: &HooksConfig,
    _skill_name: &str,
    skill_root: Option<String>,
) -> RegisterSkillHooksResult {
    let mut result = RegisterSkillHooksResult::default();

    for event in HOOK_EVENTS {
        let Some(matchers) = hooks.get(event.as_str()) else {
            continue;
        };
        for matcher in matchers {
            let matcher_value = matcher.matcher.clone().unwrap_or_default();
            for hook in &matcher.hooks {
                if hook.once == Some(true) {
                    result.one_shot_count += 1;
                }
                add_session_hook_with_skill_root(
                    session_id,
                    *event,
                    matcher_value.clone(),
                    hook.clone(),
                    skill_root.clone(),
                );
                result.registered_count += 1;
            }
        }
    }

    result
}

/// Maps to the `once: true` `onHookSuccess` callback inside CC
/// `registerSkillHooks(...)`.
pub fn remove_skill_session_hook_after_success(
    session_id: &str,
    event: HookEvent,
    hook: &HookCommand,
) {
    remove_session_hook(session_id, event, hook);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::hooks::session_hooks::{
        clear_all_session_hooks, get_session_hook_matchers, get_session_hooks,
    };

    fn config(value: serde_json::Value) -> HooksConfig {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn registers_hooks_for_official_events_with_skill_root() {
        clear_all_session_hooks();
        let hooks = config(serde_json::json!({
            "PreToolUse": [{
                "matcher": "Bash",
                "hooks": [{"type": "command", "command": "echo pre"}]
            }],
            "Stop": [{
                "hooks": [{"type": "command", "command": "echo stop", "once": true}]
            }],
            "NotARealEvent": [{
                "hooks": [{"type": "command", "command": "echo ignored"}]
            }]
        }));

        let result = register_skill_hooks(
            "session-1",
            &hooks,
            "demo-skill",
            Some("/repo/.nyan/skills/demo".to_string()),
        );

        assert_eq!(result.registered_count, 2);
        assert_eq!(result.one_shot_count, 1);
        let matchers = get_session_hook_matchers("session-1", Some(HookEvent::PreToolUse));
        let pre = matchers.get(&HookEvent::PreToolUse).expect("pre hooks");
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0].matcher, "Bash");
        assert_eq!(
            pre[0].skill_root.as_deref(),
            Some("/repo/.nyan/skills/demo")
        );
        assert_eq!(pre[0].hooks[0].command, "echo pre");
        clear_all_session_hooks();
    }

    #[test]
    fn one_shot_success_removal_uses_session_hook_identity() {
        clear_all_session_hooks();
        let hooks = config(serde_json::json!({
            "Stop": [{
                "hooks": [
                    {"type": "command", "command": "echo once", "once": true, "timeout": 1},
                    {"type": "command", "command": "echo keep"}
                ]
            }]
        }));
        register_skill_hooks("session-1", &hooks, "demo", None);
        let mut removal_hook = hooks["Stop"][0].hooks[0].clone();
        removal_hook.timeout = Some(999);

        remove_skill_session_hook_after_success("session-1", HookEvent::Stop, &removal_hook);

        let remaining = get_session_hooks("session-1", Some(HookEvent::Stop));
        let stop = remaining.get("Stop").expect("stop hooks");
        assert_eq!(stop.len(), 1);
        assert_eq!(stop[0].hooks.len(), 1);
        assert_eq!(stop[0].hooks[0].command, "echo keep");
        clear_all_session_hooks();
    }
}
