//! Maps to: CC `utils/systemPrompt.ts`.
//!
//! Assembles the effective system prompt array used by query. CLI
//! `--system-prompt` / `--append-system-prompt` land here (not AppState).

use crate::tools::agent_tool::load_agents_dir::{AgentDefinition, is_built_in_agent};
use crate::utils::feature_flags::{FeatureFlag, feature_enabled};

pub use crate::utils::system_prompt_type::{SystemPrompt, as_system_prompt};

/// Arguments for [`build_effective_system_prompt`].
///
/// Maps to: CC `utils/systemPrompt.ts#buildEffectiveSystemPrompt` params.
#[derive(Clone, Debug)]
pub struct BuildEffectiveSystemPromptArgs<'a> {
    /// Maps to CC `mainThreadAgentDefinition`.
    pub main_thread_agent_definition: Option<&'a AgentDefinition>,
    /// Maps to CC `toolUseContext` (`Pick<ToolUseContext, 'options'>`).
    /// Built-in agent prompts in Cometix are precomputed on the definition, so
    /// this is retained for API parity and future dynamic built-in prompts.
    pub tool_use_context_options: Option<&'a ToolUseContextOptions>,
    /// Maps to CC `customSystemPrompt` (`--system-prompt`).
    pub custom_system_prompt: Option<&'a str>,
    /// Maps to CC `defaultSystemPrompt`.
    pub default_system_prompt: SystemPrompt,
    /// Maps to CC `appendSystemPrompt`.
    pub append_system_prompt: Option<&'a str>,
    /// Maps to CC `overrideSystemPrompt` (e.g. loop mode — replaces all).
    pub override_system_prompt: Option<&'a str>,
}

/// Minimal options pick used by built-in `getSystemPrompt({ toolUseContext })`.
///
/// Maps to: CC `Pick<ToolUseContext, 'options'>` surface referenced by
/// `buildEffectiveSystemPrompt`.
#[derive(Clone, Debug, Default)]
pub struct ToolUseContextOptions {
    /// Maps to CC `ToolUseContext.options.mainLoopModel` (and related fields).
    pub main_loop_model: Option<String>,
}

/// Maps to: CC proactive/KAIROS lazy require + `isProactiveActive()`.
///
/// `proactive/index.ts` is a no-source stub in rebuild → always inactive.
fn is_proactive_active_safe_to_call_anywhere() -> bool {
    false
}

fn proactive_or_kairos_build_feature() -> bool {
    // Maps to CC `feature('PROACTIVE') || feature('KAIROS')`.
    feature_enabled(FeatureFlag::Proactive) || feature_enabled(FeatureFlag::Kairos)
}

fn resolve_agent_system_prompt(
    agent: &AgentDefinition,
    _tool_use_context_options: Option<&ToolUseContextOptions>,
) -> Option<String> {
    // CC built-in: `getSystemPrompt({ toolUseContext: { options } })`
    // CC custom/plugin: `getSystemPrompt()`
    // Cometix stores the resolved prompt text on `AgentDefinition.system_prompt`
    // for both (built-ins are hydrated at load time).
    let _ = is_built_in_agent(agent);
    agent.system_prompt.clone()
}

fn maybe_log_agent_memory_loaded(agent: &AgentDefinition) {
    let Some(memory) = agent.memory else {
        return;
    };
    // Maps to CC `logEvent('tengu_agent_memory_loaded', …)`. Analytics service
    // is not ported; keep a structured trace for local visibility.
    tracing::debug!(
        event = "tengu_agent_memory_loaded",
        agent_type = %agent.agent_type,
        scope = memory.official_name(),
        source = "main-thread",
        "agent memory loaded for main-thread agent"
    );
}

fn with_optional_append(mut parts: SystemPrompt, append: Option<&str>) -> SystemPrompt {
    if let Some(append) = append {
        if !append.is_empty() {
            parts.push(append.to_string());
        }
    }
    as_system_prompt(parts)
}

/// Builds the effective system prompt array based on priority:
/// 0. Override system prompt (replaces all; no append)
/// 1. Coordinator system prompt (if coordinator mode active, no main-thread agent)
/// 2. Agent system prompt (if `main_thread_agent_definition` is set)
///    - Proactive/KAIROS active: agent prompt is **appended** to default
///    - Otherwise: agent prompt **replaces** default/custom
/// 3. Custom `--system-prompt` else default
/// 4. Always append `append_system_prompt` (except when override is set)
///
/// Maps to: CC `utils/systemPrompt.ts#buildEffectiveSystemPrompt`.
pub fn build_effective_system_prompt(args: BuildEffectiveSystemPromptArgs<'_>) -> SystemPrompt {
    if let Some(override_prompt) = args.override_system_prompt {
        if !override_prompt.is_empty() {
            return as_system_prompt(vec![override_prompt.to_string()]);
        }
    }

    // Inline env + build-feature check (CC avoids importing coordinatorModule
    // here to prevent circular deps during test module loading).
    if feature_enabled(FeatureFlag::CoordinatorMode)
        && crate::utils::env_utils::is_env_truthy(
            crate::utils::process_env::var("CLAUDE_CODE_COORDINATOR_MODE").as_deref(),
        )
        && args.main_thread_agent_definition.is_none()
    {
        return with_optional_append(
            vec![crate::coordinator::coordinator_mode::get_coordinator_system_prompt()],
            args.append_system_prompt,
        );
    }

    let agent_system_prompt = args
        .main_thread_agent_definition
        .and_then(|agent| {
            maybe_log_agent_memory_loaded(agent);
            resolve_agent_system_prompt(agent, args.tool_use_context_options)
        })
        .filter(|prompt| !prompt.is_empty());

    if let Some(agent_prompt) = agent_system_prompt.as_deref() {
        if proactive_or_kairos_build_feature() && is_proactive_active_safe_to_call_anywhere() {
            let mut parts = args.default_system_prompt;
            parts.push(format!("\n# Custom Agent Instructions\n{agent_prompt}"));
            return with_optional_append(parts, args.append_system_prompt);
        }
    }

    let base: SystemPrompt = if let Some(agent_prompt) = agent_system_prompt {
        vec![agent_prompt]
    } else if let Some(custom) = args.custom_system_prompt {
        vec![custom.to_string()]
    } else {
        args.default_system_prompt
    };

    with_optional_append(base, args.append_system_prompt)
}

/// Resolve CLI system-prompt flags (inline text or file contents).
///
/// Maps to: CC `main.tsx` ~2108–2160 mutual exclusion + `readFileSync`.
pub fn resolve_cli_system_prompts(
    system_prompt: Option<&str>,
    system_prompt_file: Option<&std::path::Path>,
    append_system_prompt: Option<&str>,
    append_system_prompt_file: Option<&std::path::Path>,
) -> Result<(Option<String>, Option<String>), String> {
    if system_prompt.is_some() && system_prompt_file.is_some() {
        return Err("Cannot specify both --system-prompt and --system-prompt-file".to_string());
    }
    if append_system_prompt.is_some() && append_system_prompt_file.is_some() {
        return Err(
            "Cannot specify both --append-system-prompt and --append-system-prompt-file"
                .to_string(),
        );
    }

    let custom = if let Some(path) = system_prompt_file {
        Some(std::fs::read_to_string(path).map_err(|error| {
            format!(
                "Failed to read --system-prompt-file {}: {error}",
                path.display()
            )
        })?)
    } else {
        system_prompt.map(str::to_string)
    };

    let append = if let Some(path) = append_system_prompt_file {
        Some(std::fs::read_to_string(path).map_err(|error| {
            format!(
                "Failed to read --append-system-prompt-file {}: {error}",
                path.display()
            )
        })?)
    } else {
        append_system_prompt.map(str::to_string)
    };

    Ok((custom, append))
}

/// Session-scoped CLI overrides reconstructed from CC-aligned `ReplProps`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CliSystemPromptOverrides {
    pub custom: Option<std::sync::Arc<str>>,
    pub append: Option<std::sync::Arc<str>>,
}

impl CliSystemPromptOverrides {
    /// Apply custom/append CLI overrides onto a default system prompt.
    ///
    /// Does not set override / main-thread agent / coordinator — callers that
    /// have those should call [`build_effective_system_prompt`] directly.
    pub fn apply(&self, default: SystemPrompt) -> SystemPrompt {
        build_effective_system_prompt(BuildEffectiveSystemPromptArgs {
            main_thread_agent_definition: None,
            tool_use_context_options: None,
            custom_system_prompt: self.custom.as_deref(),
            default_system_prompt: default,
            append_system_prompt: self.append.as_deref(),
            override_system_prompt: None,
        })
    }

    /// Apply with an optional main-thread agent and override (full priority chain).
    pub fn apply_with_agent(
        &self,
        default: SystemPrompt,
        main_thread_agent: Option<&AgentDefinition>,
        override_system_prompt: Option<&str>,
        tool_use_context_options: Option<&ToolUseContextOptions>,
    ) -> SystemPrompt {
        build_effective_system_prompt(BuildEffectiveSystemPromptArgs {
            main_thread_agent_definition: main_thread_agent,
            tool_use_context_options,
            custom_system_prompt: self.custom.as_deref(),
            default_system_prompt: default,
            append_system_prompt: self.append.as_deref(),
            override_system_prompt,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.custom.is_none() && self.append.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::agent_tool::load_agents_dir::AgentDefinitionSource;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    /// Clear coordinator env so default/custom priority tests are deterministic.
    fn without_coordinator_env<T>(f: impl FnOnce() -> T) -> T {
        let _guard = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _coordinator = EnvVarGuard::unset("CLAUDE_CODE_COORDINATOR_MODE");
        f()
    }

    fn args_custom_default_append<'a>(
        custom: Option<&'a str>,
        default: SystemPrompt,
        append: Option<&'a str>,
    ) -> BuildEffectiveSystemPromptArgs<'a> {
        BuildEffectiveSystemPromptArgs {
            main_thread_agent_definition: None,
            tool_use_context_options: None,
            custom_system_prompt: custom,
            default_system_prompt: default,
            append_system_prompt: append,
            override_system_prompt: None,
        }
    }

    #[test]
    fn build_effective_prefers_custom_and_always_appends() {
        without_coordinator_env(|| {
            let prompt = build_effective_system_prompt(args_custom_default_append(
                Some("CUSTOM"),
                vec!["DEFAULT".into()],
                Some("APPEND"),
            ));
            assert_eq!(prompt, vec!["CUSTOM".to_string(), "APPEND".to_string()]);
        });
    }

    #[test]
    fn build_effective_uses_default_when_no_custom() {
        without_coordinator_env(|| {
            let prompt = build_effective_system_prompt(args_custom_default_append(
                None,
                vec!["DEFAULT".into()],
                Some("APPEND"),
            ));
            assert_eq!(prompt, vec!["DEFAULT".to_string(), "APPEND".to_string()]);
        });
    }

    #[test]
    fn build_effective_override_replaces_all_without_append() {
        without_coordinator_env(|| {
            let prompt = build_effective_system_prompt(BuildEffectiveSystemPromptArgs {
                main_thread_agent_definition: None,
                tool_use_context_options: None,
                custom_system_prompt: Some("CUSTOM"),
                default_system_prompt: vec!["DEFAULT".into()],
                append_system_prompt: Some("APPEND"),
                override_system_prompt: Some("OVERRIDE"),
            });
            assert_eq!(prompt, vec!["OVERRIDE".to_string()]);
        });
    }

    #[test]
    fn build_effective_agent_replaces_default() {
        without_coordinator_env(|| {
            let mut agent = AgentDefinition::new(
                "reviewer",
                "reviews code",
                AgentDefinitionSource::UserSettings,
            );
            agent.system_prompt = Some("AGENT_PROMPT".into());

            let prompt = build_effective_system_prompt(BuildEffectiveSystemPromptArgs {
                main_thread_agent_definition: Some(&agent),
                tool_use_context_options: None,
                custom_system_prompt: Some("CUSTOM"),
                default_system_prompt: vec!["DEFAULT".into()],
                append_system_prompt: Some("APPEND"),
                override_system_prompt: None,
            });
            assert_eq!(
                prompt,
                vec!["AGENT_PROMPT".to_string(), "APPEND".to_string()]
            );
        });
    }

    #[test]
    fn build_effective_coordinator_when_env_set_and_no_agent() {
        if !feature_enabled(FeatureFlag::CoordinatorMode) {
            return;
        }
        let _guard = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _coordinator = EnvVarGuard::set("CLAUDE_CODE_COORDINATOR_MODE", "1");
        let prompt = build_effective_system_prompt(args_custom_default_append(
            Some("CUSTOM"),
            vec!["DEFAULT".into()],
            Some("APPEND"),
        ));
        assert!(prompt[0].contains("coordinator"));
        assert_eq!(prompt.last().map(String::as_str), Some("APPEND"));
    }

    #[test]
    fn resolve_cli_reads_files_and_rejects_mutual_exclusion() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let custom_path =
            std::env::temp_dir().join(format!("cometix-sysprompt-custom-{stamp}.txt"));
        let append_path =
            std::env::temp_dir().join(format!("cometix-sysprompt-append-{stamp}.txt"));
        std::fs::write(&custom_path, "FROM_FILE").unwrap();
        std::fs::write(&append_path, "APPEND_FILE").unwrap();

        let (custom, append) = resolve_cli_system_prompts(
            None,
            Some(custom_path.as_path()),
            None,
            Some(append_path.as_path()),
        )
        .unwrap();
        assert_eq!(custom.as_deref(), Some("FROM_FILE"));
        assert_eq!(append.as_deref(), Some("APPEND_FILE"));

        assert!(
            resolve_cli_system_prompts(Some("inline"), Some(custom_path.as_path()), None, None,)
                .is_err()
        );

        let _ = std::fs::remove_file(&custom_path);
        let _ = std::fs::remove_file(&append_path);
    }

    #[test]
    fn resolve_cli_inline_text() {
        let (custom, append) =
            resolve_cli_system_prompts(Some("C"), None, Some("A"), None).unwrap();
        assert_eq!(custom.as_deref(), Some("C"));
        assert_eq!(append.as_deref(), Some("A"));
    }
}
