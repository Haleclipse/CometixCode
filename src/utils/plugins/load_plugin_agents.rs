//! Plugin agent loading.
//!
//! Maps to: CC `utils/plugins/loadPluginAgents.ts`.

use crate::tools::agent_tool::agent_memory::AgentMemoryScope;
use crate::tools::agent_tool::load_agents_dir::{AgentDefinition, AgentDefinitionSource};
use crate::types::plugin::LoadedPlugin;

use serde_json::Value;
use std::collections::HashSet;
#[cfg(test)]
use std::fs;
use std::path::{Path, PathBuf};

/// Maps to CC `loadPluginAgents.ts#loadPluginAgents`.
pub fn load_plugin_agents() -> Vec<AgentDefinition> {
    let result = super::plugin_loader::load_all_plugins_cache_only_from_sync();
    load_plugin_agents_from_plugins(&result.enabled)
}

/// Maps to CC `loadPluginAgents.ts#loadPluginAgents` after
/// `loadAllPluginsCacheOnly()` has returned enabled plugins.
pub fn load_plugin_agents_from_plugins(plugins: &[LoadedPlugin]) -> Vec<AgentDefinition> {
    let mut all_agents = Vec::new();
    for plugin in plugins.iter().filter(|plugin| plugin.enabled) {
        let mut loaded_paths = HashSet::new();
        if let Some(agents_path) = plugin.agents_path.as_ref() {
            all_agents.extend(load_agents_from_directory(
                agents_path,
                plugin,
                &mut loaded_paths,
            ));
        }
        for path in &plugin.agents_paths {
            let fs = crate::utils::fs_operations::get_fs_implementation();
            let Ok(stats) = futures::executor::block_on(fs.stat(path)) else {
                continue;
            };
            if stats.is_dir() {
                all_agents.extend(load_agents_from_directory(path, plugin, &mut loaded_paths));
            } else if stats.is_file()
                && path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
            {
                if let Some(agent) = load_agent_from_file(path, plugin, &[], &mut loaded_paths) {
                    all_agents.push(agent);
                }
            }
        }
    }
    all_agents
}

/// Maps to CC `loadPluginAgents.ts#loadAgentsFromDirectory`.
pub fn load_agents_from_directory(
    agents_path: &Path,
    plugin: &LoadedPlugin,
    loaded_paths: &mut HashSet<std::ffi::OsString>,
) -> Vec<AgentDefinition> {
    let agents_path = agents_path.to_path_buf();
    let plugin = plugin.clone();
    let shared_paths = std::sync::Arc::new(std::sync::Mutex::new(loaded_paths.clone()));
    let agents = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let callback_paths = shared_paths.clone();
    let callback_agents = agents.clone();
    crate::utils::process_runtime::block_on_from_sync(async move {
        super::walk_plugin_markdown::walk_plugin_markdown(
            &agents_path,
            move |path, namespace| {
                let plugin = plugin.clone();
                let paths = callback_paths.clone();
                let agents = callback_agents.clone();
                async move {
                    if let Some(agent) =
                        load_agent_from_file(&path, &plugin, &namespace, &mut paths.lock().unwrap())
                    {
                        agents.lock().unwrap().push(agent);
                    }
                    Ok(())
                }
            },
            super::walk_plugin_markdown::WalkPluginMarkdownOptions {
                log_label: Some("agents".to_string()),
                ..Default::default()
            },
        )
        .await;
    });
    *loaded_paths = shared_paths.lock().unwrap().clone();
    let result = agents.lock().unwrap().clone();
    result
}

/// Maps to CC `loadPluginAgents.ts#loadAgentFromFile`.
pub fn load_agent_from_file(
    file_path: &Path,
    plugin: &LoadedPlugin,
    namespace: &[String],
    loaded_paths: &mut HashSet<std::ffi::OsString>,
) -> Option<AgentDefinition> {
    let fs = crate::utils::fs_operations::get_fs_implementation();
    if crate::utils::fs_operations::is_duplicate_path(fs.as_ref(), file_path, loaded_paths) {
        return None;
    }

    let content = futures::executor::block_on(
        fs.read_file(file_path, crate::utils::fs_operations::BufferEncoding::Utf8),
    )
    .ok()?
    .to_string_lossy();
    let parsed = crate::utils::frontmatter_parser::parse_frontmatter(&content);
    let frontmatter = parsed.frontmatter;
    let markdown_content = parsed.content.trim().to_string();

    let base_agent_name = frontmatter
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            file_path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(ToOwned::to_owned)
        })?;

    let mut name_parts = Vec::with_capacity(namespace.len() + 2);
    name_parts.push(plugin.name.clone());
    name_parts.extend(namespace.iter().cloned());
    name_parts.push(base_agent_name.clone());
    let agent_type = name_parts.join(":");

    let when_to_use = crate::utils::frontmatter_parser::coerce_description_to_string(
        frontmatter.get("description"),
    )
    .or_else(|| {
        crate::utils::frontmatter_parser::coerce_description_to_string(
            frontmatter.get("when-to-use"),
        )
    })
    .unwrap_or_else(|| format!("Agent from {} plugin", plugin.name));

    let mut agent = AgentDefinition::new(
        agent_type.clone(),
        when_to_use,
        AgentDefinitionSource::Plugin,
    );
    agent.filename = Some(base_agent_name);
    agent.base_dir = file_path.parent().map(Path::to_path_buf);
    agent.plugin = Some(plugin.source.clone());
    let mut system_prompt =
        match crate::utils::plugins::plugin_options_storage::substitute_plugin_variables(
            &markdown_content,
            &plugin.path,
            Some(&plugin.source),
        ) {
            Ok(prompt) => prompt,
            Err(error) => {
                // Maps to loadAgentFromFile's catch: one failed file is omitted.
                crate::utils::debug::log_for_debugging_with_level(
                    &format!("Failed to load agent from {}: {error}", file_path.display()),
                    crate::utils::debug::DebugLogLevel::Error,
                );
                return None;
            }
        };
    if let Some(user_config) = plugin.manifest.user_config.as_ref() {
        system_prompt =
            match crate::utils::plugins::plugin_options_storage::substitute_user_config_in_content(
                &system_prompt,
                &crate::utils::plugins::plugin_options_storage::load_plugin_options(&plugin.source),
                user_config,
            ) {
                Ok(prompt) => prompt,
                Err(error) => {
                    // Source loadAgentFromFile catches String(value) failures.
                    crate::utils::debug::log_for_debugging_with_level(
                        &format!(
                            "Failed to load agent from {}: TypeError: {error}",
                            file_path.display()
                        ),
                        crate::utils::debug::DebugLogLevel::Error,
                    );
                    return None;
                }
            };
    }
    agent.system_prompt = Some(system_prompt);
    agent.tools = parse_agent_tools_from_value(frontmatter.get("tools"));
    agent.disallowed_tools = frontmatter
        .get("disallowedTools")
        .and_then(|value| parse_agent_tools_from_value(Some(value)));
    agent.skills = Some(parse_slash_command_tools_from_value(
        frontmatter.get("skills"),
    ));
    agent.color = frontmatter
        .get("color")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    agent.model = frontmatter.get("model").and_then(parse_model_value);
    agent.background = match frontmatter.get("background") {
        Some(Value::Bool(true)) => true,
        Some(Value::String(value)) if value == "true" => true,
        _ => false,
    };
    agent.memory = frontmatter
        .get("memory")
        .and_then(Value::as_str)
        .and_then(parse_agent_memory_scope);
    if agent.memory.is_some() && crate::memdir::paths::is_auto_memory_enabled() {
        inject_agent_memory_tools(&mut agent.tools);
    }
    agent.isolation = frontmatter
        .get("isolation")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| *value == "worktree")
        .map(ToOwned::to_owned);
    agent.effort = frontmatter.get("effort").and_then(parse_effort_value);
    agent.max_turns = crate::utils::frontmatter_parser::parse_positive_int_from_frontmatter(
        frontmatter.get("maxTurns"),
    );

    // Maps to CC `loadPluginAgents.ts`: permissionMode, hooks, and mcpServers
    // are intentionally not parsed for plugin agents because per-agent plugin
    // frontmatter must not escalate install-time trust.
    agent.permission_mode = None;
    agent.hooks = None;
    agent.mcp_servers = None;

    Some(agent)
}

fn parse_model_value(value: &Value) -> Option<String> {
    let model = value.as_str()?.trim();
    if model.is_empty() {
        None
    } else if model.eq_ignore_ascii_case("inherit") {
        Some("inherit".to_string())
    } else {
        Some(model.to_string())
    }
}

fn parse_effort_value(value: &Value) -> Option<crate::utils::effort::EffortValue> {
    match value {
        Value::String(value) => crate::utils::effort::parse_effort_value(value),
        Value::Number(number) => number
            .as_i64()
            .map(crate::utils::effort::EffortValue::Numeric)
            .or_else(|| {
                number
                    .as_u64()
                    .and_then(|value| i64::try_from(value).ok())
                    .map(crate::utils::effort::EffortValue::Numeric)
            }),
        _ => None,
    }
}

fn parse_agent_memory_scope(value: &str) -> Option<AgentMemoryScope> {
    match value.trim() {
        "user" => Some(AgentMemoryScope::User),
        "project" => Some(AgentMemoryScope::Project),
        "local" => Some(AgentMemoryScope::Local),
        _ => None,
    }
}

fn parse_tool_list_value(value: Option<&Value>) -> Option<Vec<String>> {
    let value = value?;
    match value {
        Value::Null => Some(Vec::new()),
        Value::Bool(false) => Some(Vec::new()),
        Value::String(value) if value.is_empty() => Some(Vec::new()),
        Value::String(value) => Some(
            crate::utils::permissions::permission_setup::parse_tool_list_from_cli(&[
                value.to_string()
            ]),
        ),
        Value::Array(values) => Some(
            crate::utils::permissions::permission_setup::parse_tool_list_from_cli(
                &values
                    .iter()
                    .filter_map(|value| value.as_str().map(ToOwned::to_owned))
                    .collect::<Vec<_>>(),
            ),
        ),
        _ => Some(Vec::new()),
    }
}

fn parse_agent_tools_from_value(value: Option<&Value>) -> Option<Vec<String>> {
    let parsed = parse_tool_list_value(value)?;
    if parsed.iter().any(|tool| tool == "*") {
        None
    } else {
        Some(parsed)
    }
}

fn parse_slash_command_tools_from_value(value: Option<&Value>) -> Vec<String> {
    parse_tool_list_value(value).unwrap_or_default()
}

fn inject_agent_memory_tools(tools: &mut Option<Vec<String>>) {
    let Some(tools) = tools.as_mut() else {
        return;
    };
    for tool in [
        crate::tools::file_write_tool::prompt::FILE_WRITE_TOOL_NAME,
        "Edit",
        crate::tools::file_read_tool::prompt::FILE_READ_TOOL_NAME,
    ] {
        if !tools.iter().any(|existing| existing == tool) {
            tools.push(tool.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::plugin::{PluginComponent, PluginError};
    use crate::utils::plugins::plugin_loader::{
        create_plugin_from_path_for_test as create_plugin_from_path, parse_marketplace_plugin_entry,
    };
    use crate::utils::plugins::schemas::PluginManifest;
    use std::io::Write;

    fn temp_dir(name: &str) -> PathBuf {
        crate::utils::process_runtime::initialize_test_process_runtime();
        let dir = std::env::temp_dir().join(format!(
            "cometix-plugin-agents-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn write_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent");
        }
        let mut file = fs::File::create(path).expect("file");
        file.write_all(content.as_bytes()).expect("write");
    }

    #[test]
    #[test]
    fn load_plugin_agents_from_default_directory_namespaces_and_ignores_trust_escalation_fields() {
        let root = temp_dir("default");
        write_file(
            &root.join(".claude-plugin/plugin.json"),
            r#"{"name":"docs-plugin"}"#,
        );
        write_file(
            &root.join("agents/reviewer.md"),
            "---\nname: reviewer\ndescription: Review docs\ntools: Read, Bash(git status, git diff)\ndisallowedTools: WebFetch\nskills: docs\ncolor: blue\nmodel: INHERIT\nbackground: true\nmemory: project\nisolation: worktree\neffort: high\nmaxTurns: 3\npermissionMode: plan\nhooks:\n  Stop:\n    - matcher: \"\"\n      hooks:\n        - command: echo bad\nmcpServers:\n  - docs\n---\nUse ${CLAUDE_PLUGIN_ROOT}/README.md",
        );

        let (plugin, errors) = create_plugin_from_path(&root, "docs-plugin@inline", true, "root");
        assert!(errors.is_empty(), "errors={errors:?}");
        let agents = load_plugin_agents_from_plugins(&[plugin]);
        assert_eq!(agents.len(), 1);
        let agent = &agents[0];
        assert_eq!(agent.agent_type, "docs-plugin:reviewer");
        assert_eq!(agent.when_to_use, "Review docs");
        assert_eq!(agent.source, AgentDefinitionSource::Plugin);
        assert_eq!(agent.plugin.as_deref(), Some("docs-plugin@inline"));
        assert_eq!(agent.filename.as_deref(), Some("reviewer"));
        assert_eq!(agent.model.as_deref(), Some("inherit"));
        assert!(agent.background);
        assert_eq!(agent.memory, Some(AgentMemoryScope::Project));
        assert_eq!(agent.isolation.as_deref(), Some("worktree"));
        assert_eq!(
            agent.effort,
            Some(crate::utils::effort::EffortValue::Named("high".to_string()))
        );
        assert_eq!(agent.max_turns, Some(3));
        let tools = agent.tools.as_ref().unwrap();
        assert_eq!(tools[0], "Read");
        assert_eq!(tools[1], "Bash(git status, git diff)");
        if crate::memdir::paths::is_auto_memory_enabled() {
            assert!(tools.contains(&"Write".to_string()));
            assert!(tools.contains(&"Edit".to_string()));
        } else {
            assert_eq!(
                tools,
                &vec!["Read".to_string(), "Bash(git status, git diff)".to_string(),]
            );
        }
        assert_eq!(
            agent.disallowed_tools.as_deref(),
            Some(&["WebFetch".to_string()][..])
        );
        assert_eq!(agent.skills.as_deref(), Some(&["docs".to_string()][..]));
        assert!(
            agent
                .system_prompt
                .as_deref()
                .expect("prompt")
                .contains(&root.display().to_string())
        );
        assert_eq!(agent.permission_mode, None);
        assert_eq!(agent.hooks, None);
        assert_eq!(agent.mcp_servers, None);
    }

    #[test]
    fn load_plugin_agents_honors_manifest_agent_paths_and_subdirectory_namespace() {
        let root = temp_dir("manifest");
        write_file(
            &root.join(".claude-plugin/plugin.json"),
            r#"{"name":"toolbox","agents":["./extra/agent.md"]}"#,
        );
        write_file(
            &root.join("agents/ignored.md"),
            "---\nname: ignored\ndescription: Ignored default dir\n---\nIgnored",
        );
        write_file(
            &root.join("extra/agent.md"),
            "---\ndescription: From manifest\n---\nManifest prompt",
        );

        let (plugin, errors) = create_plugin_from_path(&root, "toolbox@inline", true, "toolbox");
        assert!(errors.is_empty(), "errors={errors:?}");
        assert_eq!(plugin.agents_path, None);
        assert_eq!(plugin.agents_paths, vec![root.join("extra/agent.md")]);
        let agents = load_plugin_agents_from_plugins(&[plugin]);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].agent_type, "toolbox:agent");
        assert_eq!(agents[0].when_to_use, "From manifest");
    }

    #[tokio::test]
    async fn marketplace_entry_agents_are_loaded_when_plugin_manifest_is_absent() {
        let marketplace_dir = temp_dir("marketplace-no-manifest");
        let plugin_root = marketplace_dir.join("plugins/local-plugin");
        write_file(
            &plugin_root.join("custom/reviewer.md"),
            "---\nname: reviewer\ndescription: Marketplace agent\n---\nMarketplace prompt",
        );
        let entry = parse_marketplace_plugin_entry(&serde_json::json!({
            "name": "local-plugin",
            "source": "./plugins/local-plugin",
            "description": "Plugin from marketplace entry",
            "agents": "./custom/reviewer.md"
        }))
        .expect("entry");

        let (plugin, errors) =
            crate::utils::plugins::plugin_loader::load_plugin_from_marketplace_entry_cache_only(
                &entry,
                Some(&Value::String(marketplace_dir.display().to_string())),
                "local-plugin@market",
                true,
                None,
            )
            .await;

        assert!(errors.is_empty(), "errors={errors:?}");
        let plugin = plugin.expect("plugin");
        assert_eq!(plugin.name, "local-plugin");
        assert_eq!(
            plugin.manifest.description.as_deref(),
            Some("Plugin from marketplace entry")
        );
        assert_eq!(plugin.agents_path, None);
        assert_eq!(
            plugin.agents_paths,
            vec![plugin_root.join("custom/reviewer.md")]
        );
        let agents = load_plugin_agents_from_plugins(&[plugin]);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].agent_type, "local-plugin:reviewer");
        assert_eq!(agents[0].when_to_use, "Marketplace agent");
    }

    #[tokio::test]
    async fn marketplace_entry_agents_supplement_manifest_agents_in_strict_mode() {
        let marketplace_dir = temp_dir("marketplace-supplement");
        let plugin_root = marketplace_dir.join("plugins/manifest-plugin");
        write_file(
            &plugin_root.join(".claude-plugin/plugin.json"),
            r#"{"name":"manifest-plugin","agents":"./manifest/agent.md"}"#,
        );
        write_file(
            &plugin_root.join("manifest/agent.md"),
            "---\nname: manifest\ndescription: Manifest agent\n---\nManifest prompt",
        );
        write_file(
            &plugin_root.join("market/agent.md"),
            "---\nname: market\ndescription: Marketplace agent\n---\nMarketplace prompt",
        );
        let entry = parse_marketplace_plugin_entry(&serde_json::json!({
            "name": "manifest-plugin",
            "source": "./plugins/manifest-plugin",
            "strict": true,
            "agents": ["./market/agent.md"]
        }))
        .expect("entry");

        let (plugin, errors) =
            crate::utils::plugins::plugin_loader::load_plugin_from_marketplace_entry_cache_only(
                &entry,
                Some(&Value::String(marketplace_dir.display().to_string())),
                "manifest-plugin@market",
                true,
                None,
            )
            .await;

        assert!(errors.is_empty(), "errors={errors:?}");
        let plugin = plugin.expect("plugin");
        assert_eq!(
            plugin.agents_paths,
            vec![
                plugin_root.join("manifest/agent.md"),
                plugin_root.join("market/agent.md")
            ]
        );
        let agents = load_plugin_agents_from_plugins(&[plugin]);
        assert_eq!(
            agents
                .iter()
                .map(|agent| agent.agent_type.as_str())
                .collect::<Vec<_>>(),
            vec!["manifest-plugin:manifest", "manifest-plugin:market"]
        );
    }

    #[tokio::test]
    async fn marketplace_entry_agents_conflict_with_manifest_when_strict_false() {
        let marketplace_dir = temp_dir("marketplace-conflict");
        let plugin_root = marketplace_dir.join("plugins/conflict-plugin");
        write_file(
            &plugin_root.join(".claude-plugin/plugin.json"),
            r#"{"name":"conflict-plugin"}"#,
        );
        write_file(
            &plugin_root.join("market/agent.md"),
            "---\ndescription: Marketplace agent\n---\nMarketplace prompt",
        );
        let entry = parse_marketplace_plugin_entry(&serde_json::json!({
            "name": "conflict-plugin",
            "source": "./plugins/conflict-plugin",
            "strict": false,
            "agents": ["./market/agent.md"]
        }))
        .expect("entry");

        let (plugin, errors) =
            crate::utils::plugins::plugin_loader::load_plugin_from_marketplace_entry_cache_only(
                &entry,
                Some(&Value::String(marketplace_dir.display().to_string())),
                "conflict-plugin@market",
                true,
                None,
            )
            .await;

        assert!(plugin.is_none());
        assert_eq!(errors.len(), 1);
        assert!(
            crate::types::plugin::get_plugin_error_message(&errors[0])
                .contains("conflicting manifests")
        );
    }

    #[test]
    fn inject_agent_memory_tools_preserves_existing_tools_and_appends_missing_memory_tools() {
        let mut tools = Some(vec![
            crate::tools::file_read_tool::prompt::FILE_READ_TOOL_NAME.to_string(),
            "Bash(git status)".to_string(),
        ]);
        inject_agent_memory_tools(&mut tools);
        assert_eq!(
            tools.unwrap(),
            vec![
                crate::tools::file_read_tool::prompt::FILE_READ_TOOL_NAME.to_string(),
                "Bash(git status)".to_string(),
                crate::tools::file_write_tool::prompt::FILE_WRITE_TOOL_NAME.to_string(),
                "Edit".to_string(),
            ]
        );
    }
}
