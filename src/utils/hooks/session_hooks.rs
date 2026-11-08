//! Session-scoped hook registry.
//!
//! Maps to: CC `utils/hooks/sessionHooks.ts`.
//!
//! Official stores these in `AppState.sessionHooks`. Nyan does not yet carry
//! that AppState map through every runtime path, so this module preserves the
//! same public session-hook boundary in a process-local registry. Hooks are
//! still in-memory only and are cleared by the agent/skill lifecycle owner.

use crate::services::hooks::{HOOK_EVENTS, HookCommand, HookConfigEntry, HookEvent, HooksConfig};
use crate::types::message::Message;
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// Maps to: CC `FunctionHookCallback`.
pub type FunctionHookCallback = Arc<dyn Fn(Vec<Message>) -> BoxFuture<'static, bool> + Send + Sync>;

/// Maps to: CC `FunctionHook`.
#[derive(Clone)]
pub struct FunctionHook {
    pub id: Option<String>,
    /// Timeout in milliseconds, matching the official in-memory function-hook
    /// object created by `addFunctionHook`.
    pub timeout: Option<u64>,
    pub callback: FunctionHookCallback,
    pub error_message: String,
    pub status_message: Option<String>,
}

impl std::fmt::Debug for FunctionHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FunctionHook")
            .field("id", &self.id)
            .field("timeout", &self.timeout)
            .field("error_message", &self.error_message)
            .field("status_message", &self.status_message)
            .finish_non_exhaustive()
    }
}

/// Maps to: CC `addFunctionHook(...)` options object.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FunctionHookOptions {
    pub timeout: Option<u64>,
    pub id: Option<String>,
    pub status_message: Option<String>,
}

/// Maps to: CC `FunctionHookMatcher`.
#[derive(Clone, Debug)]
pub struct FunctionHookMatcher {
    pub matcher: String,
    pub hooks: Vec<FunctionHook>,
}

#[derive(Clone, Debug)]
enum SessionHookKind {
    Command(HookCommand),
    Function(FunctionHook),
}

#[derive(Clone, Debug)]
struct SessionHookEntry {
    hook: SessionHookKind,
    /// Maps to CC `SessionHookMatcher.skillRoot`. The current command-hook
    /// merge path still flattens this into `HooksConfig`; the shared hook
    /// orchestrator will consume `get_session_hook_matchers(...)` to pass
    /// `NYAN_PLUGIN_ROOT` for skill hooks.
    skill_root: Option<String>,
}

/// Maps to: CC `SessionDerivedHookMatcher`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionDerivedHookMatcher {
    pub matcher: String,
    pub hooks: Vec<HookCommand>,
    pub skill_root: Option<String>,
}

/// Maps to: CC `SessionHookMatcher`.
#[derive(Clone, Debug, Default)]
struct SessionHookMatcher {
    matcher: String,
    skill_root: Option<String>,
    hooks: Vec<SessionHookEntry>,
}

/// Maps to: CC `SessionStore`.
#[derive(Clone, Debug, Default)]
struct SessionStore {
    hooks: HashMap<HookEvent, Vec<SessionHookMatcher>>,
}

type SessionHooksState = HashMap<String, SessionStore>;

fn registry() -> &'static Mutex<SessionHooksState> {
    static REGISTRY: OnceLock<Mutex<SessionHooksState>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn add_hook_to_session(
    session_id: &str,
    event: HookEvent,
    matcher: String,
    hook: SessionHookKind,
    skill_root: Option<String>,
) {
    let mut registry = registry().lock().expect("session hooks registry poisoned");
    let store = registry.entry(session_id.to_string()).or_default();
    let event_matchers = store.hooks.entry(event).or_default();

    if let Some(existing) = event_matchers
        .iter_mut()
        .find(|entry| entry.matcher == matcher && entry.skill_root == skill_root)
    {
        existing.hooks.push(SessionHookEntry { hook, skill_root });
    } else {
        event_matchers.push(SessionHookMatcher {
            matcher,
            skill_root: skill_root.clone(),
            hooks: vec![SessionHookEntry { hook, skill_root }],
        });
    }
}

/// Maps to: CC `sessionHooks.ts#addSessionHook`.
pub fn add_session_hook(
    session_id: &str,
    event: HookEvent,
    matcher: impl Into<String>,
    hook: HookCommand,
) {
    add_hook_to_session(
        session_id,
        event,
        matcher.into(),
        SessionHookKind::Command(hook),
        None,
    );
}

/// Skill-scoped variant of `addSessionHook(...)` used by
/// `registerSkillHooks.ts`; preserves CC `skillRoot` metadata.
pub fn add_session_hook_with_skill_root(
    session_id: &str,
    event: HookEvent,
    matcher: impl Into<String>,
    hook: HookCommand,
    skill_root: Option<String>,
) {
    add_hook_to_session(
        session_id,
        event,
        matcher.into(),
        SessionHookKind::Command(hook),
        skill_root,
    );
}

/// Maps to: CC `sessionHooks.ts#addFunctionHook`.
pub fn add_function_hook(
    session_id: &str,
    event: HookEvent,
    matcher: impl Into<String>,
    callback: FunctionHookCallback,
    error_message: impl Into<String>,
    options: Option<FunctionHookOptions>,
) -> String {
    let options = options.unwrap_or_default();
    let id = options
        .id
        .unwrap_or_else(|| format!("function-hook-{}", uuid::Uuid::new_v4()));
    let hook = FunctionHook {
        id: Some(id.clone()),
        timeout: Some(options.timeout.unwrap_or(5_000)),
        callback,
        error_message: error_message.into(),
        status_message: options.status_message,
    };
    add_hook_to_session(
        session_id,
        event,
        matcher.into(),
        SessionHookKind::Function(hook),
        None,
    );
    id
}

/// Maps to: CC `sessionHooks.ts#removeFunctionHook`.
pub fn remove_function_hook(session_id: &str, event: HookEvent, hook_id: &str) {
    let mut registry = registry().lock().expect("session hooks registry poisoned");
    let Some(store) = registry.get_mut(session_id) else {
        return;
    };
    let Some(event_matchers) = store.hooks.get_mut(&event) else {
        return;
    };

    for matcher in event_matchers.iter_mut() {
        matcher.hooks.retain(|entry| match &entry.hook {
            SessionHookKind::Function(function_hook) => {
                function_hook.id.as_deref() != Some(hook_id)
            }
            SessionHookKind::Command(_) => true,
        });
    }
    event_matchers.retain(|matcher| !matcher.hooks.is_empty());
    if event_matchers.is_empty() {
        store.hooks.remove(&event);
    }
}

fn convert_to_hook_matchers(
    session_matchers: &[SessionHookMatcher],
) -> Vec<SessionDerivedHookMatcher> {
    session_matchers
        .iter()
        .map(|matcher| SessionDerivedHookMatcher {
            matcher: matcher.matcher.clone(),
            hooks: matcher
                .hooks
                .iter()
                .filter_map(|entry| match &entry.hook {
                    SessionHookKind::Command(hook) => Some(hook.clone()),
                    SessionHookKind::Function(_) => None,
                })
                .collect(),
            skill_root: matcher.skill_root.clone(),
        })
        .filter(|matcher| !matcher.hooks.is_empty())
        .collect()
}

fn extract_function_hook_matchers(
    session_matchers: &[SessionHookMatcher],
) -> Vec<FunctionHookMatcher> {
    session_matchers
        .iter()
        .map(|matcher| FunctionHookMatcher {
            matcher: matcher.matcher.clone(),
            hooks: matcher
                .hooks
                .iter()
                .filter_map(|entry| match &entry.hook {
                    SessionHookKind::Function(hook) => Some(hook.clone()),
                    SessionHookKind::Command(_) => None,
                })
                .collect(),
        })
        .filter(|matcher| !matcher.hooks.is_empty())
        .collect()
}

/// Maps to: CC `sessionHooks.ts#getSessionHooks` preserving `skillRoot`.
pub fn get_session_hook_matchers(
    session_id: &str,
    event: Option<HookEvent>,
) -> HashMap<HookEvent, Vec<SessionDerivedHookMatcher>> {
    let registry = registry().lock().expect("session hooks registry poisoned");
    let Some(store) = registry.get(session_id) else {
        return HashMap::new();
    };

    if let Some(event) = event {
        return store
            .hooks
            .get(&event)
            .map(|entries| HashMap::from([(event, convert_to_hook_matchers(entries))]))
            .unwrap_or_default();
    }

    let mut result = HashMap::new();
    for event in HOOK_EVENTS {
        if let Some(entries) = store.hooks.get(event) {
            let converted = convert_to_hook_matchers(entries);
            if !converted.is_empty() {
                result.insert(*event, converted);
            }
        }
    }
    result
}

/// Maps to: CC `sessionHooks.ts#getSessionFunctionHooks`.
pub fn get_session_function_hooks(
    session_id: &str,
    event: Option<HookEvent>,
) -> HashMap<HookEvent, Vec<FunctionHookMatcher>> {
    let registry = registry().lock().expect("session hooks registry poisoned");
    let Some(store) = registry.get(session_id) else {
        return HashMap::new();
    };

    if let Some(event) = event {
        return store
            .hooks
            .get(&event)
            .map(|entries| extract_function_hook_matchers(entries))
            .filter(|matchers| !matchers.is_empty())
            .map(|matchers| HashMap::from([(event, matchers)]))
            .unwrap_or_default();
    }

    let mut result = HashMap::new();
    for event in HOOK_EVENTS {
        if let Some(entries) = store.hooks.get(event) {
            let converted = extract_function_hook_matchers(entries);
            if !converted.is_empty() {
                result.insert(*event, converted);
            }
        }
    }
    result
}

fn to_hooks_config(
    matchers_by_event: HashMap<HookEvent, Vec<SessionDerivedHookMatcher>>,
) -> HooksConfig {
    matchers_by_event
        .into_iter()
        .map(|(event, matchers)| {
            (
                event.as_str().to_string(),
                matchers
                    .into_iter()
                    .map(|matcher| HookConfigEntry {
                        matcher: if matcher.matcher.is_empty() {
                            None
                        } else {
                            Some(matcher.matcher)
                        },
                        hooks: matcher.hooks,
                        plugin_root: None,
                        plugin_name: None,
                        plugin_id: None,
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

/// Maps to: CC `sessionHooks.ts#getSessionHooks` for command hooks.
pub fn get_session_hooks(session_id: &str, event: Option<HookEvent>) -> HooksConfig {
    to_hooks_config(get_session_hook_matchers(session_id, event))
}

/// Maps to the session-hook merge step inside CC `utils/hooks.ts#getMatchingHooks`.
pub fn merge_session_hooks_into_config(
    config: &mut crate::schemas::hooks::RegisteredHooks,
    session_id: &str,
) {
    for (event, entries) in get_session_hooks(session_id, None) {
        config.entry(event).or_default().extend(
            entries
                .iter()
                .map(crate::schemas::hooks::RegisteredHookMatcher::from_config_entry),
        );
    }
}

/// Maps to: CC `sessionHooks.ts#removeSessionHook`.
pub fn remove_session_hook(session_id: &str, event: HookEvent, hook: &HookCommand) {
    let mut registry = registry().lock().expect("session hooks registry poisoned");
    let Some(store) = registry.get_mut(session_id) else {
        return;
    };
    let Some(event_matchers) = store.hooks.get_mut(&event) else {
        return;
    };

    for matcher in event_matchers.iter_mut() {
        matcher.hooks.retain(|entry| match &entry.hook {
            SessionHookKind::Command(entry_hook) => {
                !super::hooks_settings::is_hook_equal(entry_hook, hook)
            }
            // Official `isHookEqual` intentionally cannot compare function hooks;
            // `removeFunctionHook` removes those by stable ID instead.
            SessionHookKind::Function(_) => true,
        });
    }
    event_matchers.retain(|matcher| !matcher.hooks.is_empty());
    if event_matchers.is_empty() {
        store.hooks.remove(&event);
    }
}

/// Maps to: CC `sessionHooks.ts#clearSessionHooks`.
pub fn clear_session_hooks(session_id: &str) {
    let mut registry = registry().lock().expect("session hooks registry poisoned");
    registry.remove(session_id);
}

/// Test/lifecycle helper mirroring session hook map cleanup semantics.
#[cfg(test)]
pub fn clear_all_session_hooks() {
    registry()
        .lock()
        .expect("session hooks registry poisoned")
        .clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook(command: &str) -> HookCommand {
        HookCommand {
            command: command.to_string(),
            shell: None,
            timeout: Some(5),
            condition: None,
            status: None,
            once: None,
            is_async: None,
            async_rewake: None,
        }
    }

    fn callback(result: bool) -> FunctionHookCallback {
        Arc::new(move |_messages| Box::pin(async move { result }))
    }

    #[test]
    fn session_hooks_merge_by_session_and_event_like_official_store() {
        clear_all_session_hooks();
        add_session_hook("agent-1", HookEvent::PreToolUse, "Bash", hook("echo pre"));
        add_session_hook("agent-2", HookEvent::PreToolUse, "Bash", hook("echo other"));

        let mut config = crate::schemas::hooks::RegisteredHooks::new();
        merge_session_hooks_into_config(&mut config, "agent-1");
        let pre = config.get("PreToolUse").expect("pre hooks");
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0].matcher.as_deref(), Some("Bash"));
        assert!(matches!(
            &pre[0].hooks[0],
            crate::schemas::hooks::RegisteredHook::Command(hook) if hook.command == "echo pre"
        ));

        clear_session_hooks("agent-1");
        assert!(get_session_hooks("agent-1", None).is_empty());
        assert!(!get_session_hooks("agent-2", None).is_empty());
        clear_all_session_hooks();
    }

    #[test]
    fn skill_root_partitions_matchers_like_official_session_hook_store() {
        clear_all_session_hooks();
        add_session_hook_with_skill_root(
            "s1",
            HookEvent::Stop,
            "",
            hook("echo one"),
            Some("/skills/one".to_string()),
        );
        add_session_hook_with_skill_root(
            "s1",
            HookEvent::Stop,
            "",
            hook("echo two"),
            Some("/skills/two".to_string()),
        );
        add_session_hook_with_skill_root(
            "s1",
            HookEvent::Stop,
            "",
            hook("echo again"),
            Some("/skills/one".to_string()),
        );

        let matchers = get_session_hook_matchers("s1", Some(HookEvent::Stop));
        let stop = matchers.get(&HookEvent::Stop).expect("stop matchers");
        assert_eq!(stop.len(), 2);
        let one = stop
            .iter()
            .find(|matcher| matcher.skill_root.as_deref() == Some("/skills/one"))
            .expect("skill one matcher");
        assert_eq!(one.hooks.len(), 2);
        clear_all_session_hooks();
    }

    #[test]
    fn remove_session_hook_uses_official_hook_identity_without_timeout() {
        clear_all_session_hooks();
        let mut to_remove = hook("echo same");
        to_remove.timeout = Some(1);
        let mut stored = hook("echo same");
        stored.timeout = Some(99);
        add_session_hook("s1", HookEvent::PreToolUse, "Bash", stored);
        add_session_hook("s1", HookEvent::PreToolUse, "Bash", hook("echo keep"));

        remove_session_hook("s1", HookEvent::PreToolUse, &to_remove);

        let hooks = get_session_hooks("s1", Some(HookEvent::PreToolUse));
        let pre = hooks.get("PreToolUse").expect("pre hooks");
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0].hooks.len(), 1);
        assert_eq!(pre[0].hooks[0].command, "echo keep");
        clear_all_session_hooks();
    }

    #[test]
    fn function_hooks_are_kept_separate_from_command_hook_config_like_official() {
        clear_all_session_hooks();
        add_session_hook("s1", HookEvent::Stop, "", hook("echo stop"));
        let id = add_function_hook(
            "s1",
            HookEvent::Stop,
            "",
            callback(true),
            "must call StructuredOutput",
            Some(FunctionHookOptions {
                timeout: Some(1234),
                id: Some("fn-1".to_string()),
                status_message: Some("checking".to_string()),
            }),
        );
        assert_eq!(id, "fn-1");

        let command_hooks = get_session_hooks("s1", Some(HookEvent::Stop));
        assert_eq!(command_hooks["Stop"][0].hooks.len(), 1);
        assert_eq!(command_hooks["Stop"][0].hooks[0].command, "echo stop");

        let function_hooks = get_session_function_hooks("s1", Some(HookEvent::Stop));
        let matcher = &function_hooks[&HookEvent::Stop][0];
        assert_eq!(matcher.matcher, "");
        assert_eq!(matcher.hooks.len(), 1);
        assert_eq!(matcher.hooks[0].id.as_deref(), Some("fn-1"));
        assert_eq!(matcher.hooks[0].timeout, Some(1234));
        assert_eq!(matcher.hooks[0].status_message.as_deref(), Some("checking"));
        clear_all_session_hooks();
    }

    #[tokio::test]
    async fn remove_function_hook_uses_id_and_leaves_command_hooks() {
        clear_all_session_hooks();
        add_session_hook("s1", HookEvent::Stop, "", hook("echo stop"));
        add_function_hook(
            "s1",
            HookEvent::Stop,
            "",
            callback(true),
            "one",
            Some(FunctionHookOptions {
                id: Some("fn-1".to_string()),
                ..Default::default()
            }),
        );
        add_function_hook(
            "s1",
            HookEvent::Stop,
            "Bash",
            callback(false),
            "two",
            Some(FunctionHookOptions {
                id: Some("fn-2".to_string()),
                ..Default::default()
            }),
        );

        remove_function_hook("s1", HookEvent::Stop, "fn-1");

        let command_hooks = get_session_hooks("s1", Some(HookEvent::Stop));
        assert_eq!(command_hooks["Stop"][0].hooks[0].command, "echo stop");
        let function_hooks = get_session_function_hooks("s1", Some(HookEvent::Stop));
        let stop = function_hooks.get(&HookEvent::Stop).expect("function stop");
        assert_eq!(stop.len(), 1);
        assert_eq!(stop[0].matcher, "Bash");
        assert_eq!(stop[0].hooks[0].id.as_deref(), Some("fn-2"));
        assert!(!(stop[0].hooks[0].callback)(Vec::new()).await);
        clear_all_session_hooks();
    }
}
