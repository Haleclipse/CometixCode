//! Hook config matching.
//! Maps to: CC `utils/hooks.ts:1346-1459,1603-1880` (`matchesPattern`,
//! `prepareIfConditionMatcher`, `hookDedupKey`, and `getMatchingHooks`).
//!
//! User-authorized structural decomposition: CC keeps these functions in its
//! ~5k-line hook monolith; Rust keeps matcher policy in `services/hooks`.

use super::{HookConfigEntry, HookEvent};
use crate::schemas::hooks::{RegisteredHook, RegisteredHooks};

/// A matched hook ready for execution.
/// Maps to: CC `MatchedHook` type — `hook` carries the
/// `HookCommand | HookCallback` union the execution chain dispatches on
/// (hooks.ts:356).
#[derive(Debug, Clone)]
pub struct MatchedHook {
    pub hook: RegisteredHook,
    pub hook_source: Option<String>,
    pub plugin_root: Option<String>,
    pub plugin_id: Option<String>,
}

/// Maps to: CC `utils/hooks.ts:1346-1382#matchesPattern`.
///
/// User-authorized structural decomposition: the direct-transformed matcher
/// remains in the Rust hook-matching service.
pub fn matches_pattern(match_query: &str, matcher: &str) -> bool {
    if matcher.is_empty() || matcher == "*" {
        return true;
    }

    // CC's exact-match fast path accepts only ASCII letters, digits,
    // underscore, and pipe. Whitespace therefore selects RegExp semantics.
    let is_simple = matcher
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'|');
    if is_simple {
        return matcher.split('|').any(|pattern| {
            crate::utils::permissions::permission_rule_parser::normalize_legacy_tool_name(pattern)
                == match_query
        });
    }

    // CC constructs an ECMAScript RegExp without the `u` flag. `regress`
    // supplies the required syntax (including lookarounds/backreferences), and
    // its UCS-2 input path preserves JavaScript's UTF-16 code-unit matching.
    let Ok(regex) = regress::Regex::from_unicode(
        matcher.encode_utf16().map(u32::from),
        regress::Flags::default(),
    ) else {
        crate::utils::debug::log_for_debugging(&format!(
            "Invalid regex pattern in hook matcher: {matcher}"
        ));
        return false;
    };
    let matches = |candidate: &str| {
        let code_units = candidate.encode_utf16().collect::<Vec<_>>();
        regex.find_from_ucs2(&code_units, 0).next().is_some()
    };
    matches(match_query)
        || crate::utils::permissions::permission_rule_parser::get_legacy_tool_names(match_query)
            .iter()
            .any(|legacy_name| matches(legacy_name))
}

/// Maps to: CC `utils/hooks.ts:1387#IfConditionMatcher`.
type IfConditionMatcher = Box<dyn Fn(&str) -> bool + Send + Sync>;

/// Maps to: CC `utils/hooks.ts:1390-1425#prepareIfConditionMatcher`.
fn prepare_if_condition_matcher(
    event: HookEvent,
    tool_name: &str,
    tool_input: Option<&serde_json::Value>,
) -> Option<IfConditionMatcher> {
    if !matches!(
        event,
        HookEvent::PreToolUse
            | HookEvent::PostToolUse
            | HookEvent::PostToolUseFailure
            | HookEvent::PermissionRequest
    ) {
        return None;
    }

    let normalized_tool_name =
        crate::utils::permissions::permission_rule_parser::normalize_legacy_tool_name(tool_name);
    let pattern_matcher = tool_input.and_then(|tool_input| {
        let tool = crate::services::tools::tool_execution::find_tool_call(tool_name)?;
        let decision_input = tool_input.clone();
        let normalized_input = tool.normalize_input(&decision_input);
        let definitions = crate::tools::get_all_base_tools();
        let definition = crate::types::tools::find_tool_by_name(&definitions, tool_name)
            .or_else(|| crate::types::tools::find_tool_by_name(&definitions, tool.name()))?;
        let validator = jsonschema::draft7::new(&definition.input_schema).ok()?;
        if !validator.is_valid(&normalized_input) {
            return None;
        }
        tool.prepare_permission_matcher(&normalized_input)
    });

    Some(Box::new(move |if_condition| {
        let parsed =
            crate::utils::permissions::permission_rule_parser::permission_rule_value_from_string(
                if_condition,
            );
        if crate::utils::permissions::permission_rule_parser::normalize_legacy_tool_name(
            &parsed.tool_name,
        ) != normalized_tool_name
        {
            return false;
        }
        let Some(rule_content) = parsed
            .rule_content
            .as_deref()
            .filter(|content| !content.is_empty())
        else {
            return true;
        };
        pattern_matcher
            .as_ref()
            .is_some_and(|matcher| matcher(rule_content))
    }))
}

/// Get matching hooks for an event from the hooks config.
/// Maps to: CC `utils/hooks.ts:1603-1880#getMatchingHooks`.
///
/// Looks up the event key in the config, filters entries by matcher against
/// the provided match query, and filters hook `if` conditions against the
/// optional current Tool input.
pub fn get_matching_hooks(
    config: &RegisteredHooks,
    event: HookEvent,
    match_query: &str,
    tool_input: Option<&serde_json::Value>,
) -> Vec<MatchedHook> {
    crate::utils::debug::log_for_debugging(&format!(
        "Getting matching hooks for event={:?} query={match_query}",
        event
    ));
    let event_key = event.as_str();
    let entries = match config.get(event_key) {
        Some(entries) => entries,
        None => return Vec::new(),
    };

    let mut matched = Vec::new();
    for entry in entries {
        let pattern = entry.matcher.as_deref().unwrap_or("*");
        // CC skips top-level matcher filtering when `matchQuery` is falsey.
        // Events such as Stop, UserPromptSubmit, and WorktreeCreate therefore
        // retain configured matcher text rather than matching it against `""`.
        if match_query.is_empty() || matches_pattern(match_query, pattern) {
            for hook in &entry.hooks {
                matched.push(MatchedHook {
                    hook: hook.clone(),
                    hook_source: Some(if entry.plugin_root.is_some() {
                        entry
                            .plugin_name
                            .as_ref()
                            .map(|name| format!("plugin:{name}"))
                            .unwrap_or_else(|| "plugin".to_string())
                    } else {
                        "settings".to_string()
                    }),
                    plugin_root: entry.plugin_root.clone(),
                    plugin_id: entry.plugin_id.clone(),
                });
            }
        }
    }

    // CC deduplicates COMMAND hooks by source root, effective shell, command,
    // and `if`, with Map semantics: first-key order but the last matching entry
    // supplies the retained value. Callback hooks never dedup — CC filters
    // them into their own branch (hooks.ts:1796) — so each gets a unique key
    // that preserves its position.
    let mut unique = indexmap::IndexMap::<String, MatchedHook>::new();
    for (index, entry) in matched.into_iter().enumerate() {
        let key = match &entry.hook {
            RegisteredHook::Command(command) => {
                let payload = format!(
                    "{}\0{}\0{}",
                    command.shell.as_deref().unwrap_or("bash"),
                    command.command,
                    command.condition.as_deref().unwrap_or("")
                );
                hook_dedup_key(&entry, &payload)
            }
            RegisteredHook::Callback(_) => format!("\0callback\0{index}"),
        };
        unique.insert(key, entry);
    }
    let mut matched = unique.into_values().collect::<Vec<_>>();

    let command_condition = |entry: &MatchedHook| -> Option<String> {
        match &entry.hook {
            RegisteredHook::Command(command) => command
                .condition
                .clone()
                .filter(|condition| !condition.is_empty()),
            // `if` is a command-hook field; callbacks always run.
            RegisteredHook::Callback(_) => None,
        }
    };
    let has_if_condition = matched
        .iter()
        .any(|entry| command_condition(entry).is_some());
    if has_if_condition {
        let if_matcher = prepare_if_condition_matcher(event, match_query, tool_input);
        matched.retain(|entry| {
            let Some(condition) = command_condition(entry) else {
                return true;
            };
            if_matcher
                .as_ref()
                .is_some_and(|matcher| matcher(&condition))
        });
    }

    crate::utils::debug::log_for_debugging(&format!(
        "Matched {} unique hooks for event={:?}",
        matched.len(),
        event
    ));
    matched
}

/// Dedup key for a matched hook.
/// Maps to the plugin-root/settings branch of CC `hookDedupKey()`
/// (hooks.ts:1453-1460). `HooksConfig` does not yet carry `skillRoot` into
/// `MatchedHook`, so skill-root namespacing remains an explicit config seam.
pub fn hook_dedup_key(matched: &MatchedHook, payload: &str) -> String {
    let prefix = matched.plugin_root.as_deref().unwrap_or("");
    format!("{}\0{}", prefix, payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::hooks::{HookCommand, HooksConfig, test_support::registered_config};

    /// Test-side shim: fixtures stay in the settings-shaped `HooksConfig` (so
    /// they can be mutated field-by-field) and are folded into the
    /// execution-facing `RegisteredHooks` table at the call boundary, exactly
    /// like the loading chain. Shadows `super::get_matching_hooks`.
    fn get_matching_hooks(
        config: &HooksConfig,
        event: HookEvent,
        match_query: &str,
        tool_input: Option<&serde_json::Value>,
    ) -> Vec<MatchedHook> {
        super::get_matching_hooks(&registered_config(config), event, match_query, tool_input)
    }

    #[test]
    fn top_level_matching_matches_official_case_whitespace_pipe_regex_and_legacy_names() {
        assert!(matches_pattern("Bash", "*"));
        assert!(matches_pattern("Bash", ""));

        assert!(matches_pattern("Bash", "Bash"));
        assert!(!matches_pattern("bash", "Bash"));
        assert!(!matches_pattern("Bash", " Bash "));
        assert!(!matches_pattern("Bash", " "));

        assert!(matches_pattern("Read", "Bash|Read|Write"));
        assert!(!matches_pattern("Grep", "Bash|Read|Write"));
        assert!(matches_pattern(
            crate::tools::agent_tool::constants::AGENT_TOOL_NAME,
            "Task|Read"
        ));

        assert!(matches_pattern("Bash", "^Ba.*$"));
        assert!(!matches_pattern("bash", "^Ba.*$"));
        assert!(matches_pattern("Bash", r"^B(?=ash)ash$"));
        assert!(matches_pattern("aa", r"(?<=a)a"));
        assert!(matches_pattern("BashBash", r"^(Bash)\1$"));
        assert!(matches_pattern(
            crate::tools::agent_tool::constants::AGENT_TOOL_NAME,
            "^Task$"
        ));
        assert!(!matches_pattern("😀", r"^.$"));
        assert!(matches_pattern("😀", r"^😀$"));
        assert!(!matches_pattern("Read", "["));
    }

    fn conditional_hook_for_event(event: HookEvent, matcher: &str, condition: &str) -> HooksConfig {
        std::collections::HashMap::from([(
            event.as_str().to_string(),
            vec![HookConfigEntry {
                matcher: Some(matcher.to_string()),
                hooks: vec![HookCommand {
                    command: "echo hook".to_string(),
                    shell: None,
                    timeout: None,
                    condition: Some(condition.to_string()),
                    status: None,
                    once: None,
                    is_async: None,
                    async_rewake: None,
                }],
                plugin_root: None,
                plugin_name: None,
                plugin_id: None,
            }],
        )])
    }

    fn conditional_hook(condition: &str) -> HooksConfig {
        conditional_hook_for_event(HookEvent::PreToolUse, "Bash", condition)
    }

    #[test]
    fn bash_if_condition_uses_prepared_tool_matcher_and_fails_safe_without_parser() {
        let config = conditional_hook("Bash(git *)");
        let git = get_matching_hooks(
            &config,
            HookEvent::PreToolUse,
            "Bash",
            Some(&serde_json::json!({"command": "git push"})),
        );
        assert_eq!(git.len(), 1);

        let list = get_matching_hooks(
            &config,
            HookEvent::PreToolUse,
            "Bash",
            Some(&serde_json::json!({"command": "ls"})),
        );
        assert_eq!(
            list.len(),
            1,
            "external CC runs the hook when tree-sitter is parse-unavailable"
        );

        let wrong_tool = conditional_hook("Read(**)");
        assert!(
            get_matching_hooks(
                &wrong_tool,
                HookEvent::PreToolUse,
                "Bash",
                Some(&serde_json::json!({"command": "git push"})),
            )
            .is_empty()
        );
    }

    #[test]
    fn glob_if_condition_uses_tool_owned_wildcard_matcher() {
        let mut config = conditional_hook("Glob(src/**)");
        config.get_mut(HookEvent::PreToolUse.as_str()).unwrap()[0].matcher =
            Some("Glob".to_string());
        assert_eq!(
            get_matching_hooks(
                &config,
                HookEvent::PreToolUse,
                "Glob",
                Some(&serde_json::json!({"pattern": "src/**/*.rs"})),
            )
            .len(),
            1
        );
        assert!(
            get_matching_hooks(
                &config,
                HookEvent::PreToolUse,
                "Glob",
                Some(&serde_json::json!({"pattern": "tests/**/*.rs"})),
            )
            .is_empty()
        );
    }

    #[test]
    fn read_if_condition_schema_gate_matches_official_safe_parse() {
        let mut config = conditional_hook("Read(/tmp/**)");
        config.get_mut(HookEvent::PreToolUse.as_str()).unwrap()[0].matcher =
            Some("Read".to_string());

        assert_eq!(
            get_matching_hooks(
                &config,
                HookEvent::PreToolUse,
                "Read",
                Some(&serde_json::json!({"file_path": "/tmp/source.txt", "offset": "2"})),
            )
            .len(),
            1,
            "safeParse applies semanticNumber before preparing the Tool matcher"
        );
        assert!(
            get_matching_hooks(
                &config,
                HookEvent::PreToolUse,
                "Read",
                Some(&serde_json::json!({"file_path": 5})),
            )
            .is_empty()
        );
        assert!(
            get_matching_hooks(
                &config,
                HookEvent::PreToolUse,
                "Read",
                Some(&serde_json::json!({"file_path": "/tmp/source.txt", "unknown": true})),
            )
            .is_empty()
        );

        let tool_level = conditional_hook_for_event(HookEvent::PreToolUse, "Read", "Read");
        assert_eq!(
            get_matching_hooks(
                &tool_level,
                HookEvent::PreToolUse,
                "Read",
                Some(&serde_json::json!({"file_path": 5})),
            )
            .len(),
            1,
            "a schema failure disables only the Tool-specific content matcher"
        );
    }

    #[test]
    fn empty_rule_content_matches_official_tool_level_truthiness() {
        for condition in ["Read()", "Read(*)"] {
            let config = conditional_hook_for_event(HookEvent::PreToolUse, "Read", condition);
            assert_eq!(
                get_matching_hooks(
                    &config,
                    HookEvent::PreToolUse,
                    "Read",
                    Some(&serde_json::json!({"file_path": 5})),
                )
                .len(),
                1,
                "{condition} must remain a tool-level match"
            );
        }
    }

    #[test]
    fn unsupported_event_if_conditions_match_official_rejection() {
        let permission_denied =
            conditional_hook_for_event(HookEvent::PermissionDenied, "Read", "Read");
        assert!(
            get_matching_hooks(
                &permission_denied,
                HookEvent::PermissionDenied,
                "Read",
                Some(&serde_json::json!({"file_path": "/tmp/source.txt"})),
            )
            .is_empty()
        );

        let stop = conditional_hook_for_event(HookEvent::Stop, "Read", "Read");
        assert!(get_matching_hooks(&stop, HookEvent::Stop, "", None).is_empty());
    }

    #[test]
    fn falsey_match_query_keeps_configured_matchers_like_official() {
        let mut stop = conditional_hook_for_event(HookEvent::Stop, "Read", "ignored");
        stop.get_mut(HookEvent::Stop.as_str()).unwrap()[0].hooks[0].condition = None;

        assert_eq!(
            get_matching_hooks(&stop, HookEvent::Stop, "", None).len(),
            1,
            "CC does not apply a top-level matcher when the event has no match query"
        );
    }

    #[test]
    fn command_dedup_uses_source_root_shell_command_condition_and_last_value() {
        let hook = |shell: Option<&str>, condition: Option<&str>, timeout| HookCommand {
            command: "echo hook".to_string(),
            shell: shell.map(str::to_string),
            timeout: Some(timeout),
            condition: condition.map(str::to_string),
            status: None,
            once: None,
            is_async: None,
            async_rewake: None,
        };
        let entry = |plugin_root: Option<&str>,
                     plugin_name: Option<&str>,
                     hooks: Vec<HookCommand>| HookConfigEntry {
            matcher: Some("Read".to_string()),
            hooks,
            plugin_root: plugin_root.map(str::to_string),
            plugin_name: plugin_name.map(str::to_string),
            plugin_id: None,
        };
        let config = std::collections::HashMap::from([(
            HookEvent::PostToolUse.as_str().to_string(),
            vec![
                entry(None, None, vec![hook(None, None, 1)]),
                entry(None, None, vec![hook(Some("bash"), None, 2)]),
                entry(None, None, vec![hook(Some("powershell"), None, 3)]),
                entry(None, None, vec![hook(None, Some("Read"), 4)]),
                entry(Some("/plugins/one"), Some("one"), vec![hook(None, None, 5)]),
                entry(Some("/plugins/two"), Some("two"), vec![hook(None, None, 6)]),
                entry(
                    Some("/plugins/one"),
                    Some("renamed-one"),
                    vec![hook(None, None, 7)],
                ),
            ],
        )]);

        let matched = get_matching_hooks(
            &config,
            HookEvent::PostToolUse,
            "Read",
            Some(&serde_json::json!({"file_path": "/tmp/source.txt"})),
        );

        assert_eq!(matched.len(), 5);
        assert_eq!(matched[0].hook.timeout(), Some(2));
        assert_eq!(matched[0].hook_source.as_deref(), Some("settings"));
        assert_eq!(matched[3].hook.timeout(), Some(7));
        assert_eq!(matched[3].plugin_root.as_deref(), Some("/plugins/one"));
        assert_eq!(
            matched[3].hook_source.as_deref(),
            Some("plugin:renamed-one")
        );
        assert_eq!(matched[4].plugin_root.as_deref(), Some("/plugins/two"));
    }
}
