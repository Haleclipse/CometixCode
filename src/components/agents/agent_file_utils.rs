//! Maps to: CC `components/agents/agentFileUtils.ts:1-274`.

use crate::tools::agent_tool::agent_memory::AgentMemoryScope;
use crate::tools::agent_tool::load_agents_dir::{AgentDefinition, AgentDefinitionSource};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentFileSource {
    UserSettings,
    ProjectSettings,
    LocalSettings,
    PolicySettings,
    FlagSettings,
    BuiltIn,
    Plugin,
}

fn source_from_definition(source: AgentDefinitionSource) -> AgentFileSource {
    match source {
        AgentDefinitionSource::UserSettings => AgentFileSource::UserSettings,
        AgentDefinitionSource::ProjectSettings => AgentFileSource::ProjectSettings,
        AgentDefinitionSource::PolicySettings => AgentFileSource::PolicySettings,
        AgentDefinitionSource::FlagSettings => AgentFileSource::FlagSettings,
        AgentDefinitionSource::BuiltIn => AgentFileSource::BuiltIn,
        AgentDefinitionSource::Plugin => AgentFileSource::Plugin,
    }
}

pub fn format_agent_as_markdown(
    agent_type: &str,
    when_to_use: &str,
    tools: Option<&[String]>,
    system_prompt: &str,
    color: Option<&str>,
    model: Option<&str>,
    memory: Option<AgentMemoryScope>,
) -> String {
    let escaped = when_to_use
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\\\n");
    let all_tools = tools.is_none() || tools.is_some_and(|tools| tools == ["*".to_string()]);
    let tools_line = if all_tools {
        String::new()
    } else {
        format!("\ntools: {}", tools.unwrap_or_default().join(", "))
    };
    let model_line = model
        .map(|value| format!("\nmodel: {value}"))
        .unwrap_or_default();
    let color_line = color
        .map(|value| format!("\ncolor: {value}"))
        .unwrap_or_default();
    let memory_line = memory
        .map(|value| format!("\nmemory: {}", value.official_name()))
        .unwrap_or_default();
    format!(
        "---\nname: {agent_type}\ndescription: \"{escaped}\"{tools_line}{model_line}{color_line}{memory_line}\n---\n\n{system_prompt}\n"
    )
}

fn agent_directory(
    source: AgentFileSource,
    cwd: &Path,
    config_home: &Path,
) -> Result<PathBuf, String> {
    match source {
        AgentFileSource::UserSettings => Ok(config_home.join("agents")),
        AgentFileSource::ProjectSettings | AgentFileSource::LocalSettings => {
            Ok(cwd.join(".nyan").join("agents"))
        }
        AgentFileSource::PolicySettings => Ok(
            crate::utils::settings::managed_path::get_managed_file_path()
                .join(".nyan")
                .join("agents"),
        ),
        AgentFileSource::FlagSettings => {
            Err("Cannot get directory path for flagSettings agents".to_string())
        }
        AgentFileSource::BuiltIn => Err("Cannot save built-in agents".to_string()),
        AgentFileSource::Plugin => Err("Cannot get file path for plugin agents".to_string()),
    }
}

pub fn get_new_agent_file_path(
    source: AgentFileSource,
    agent_type: &str,
    cwd: &Path,
    config_home: &Path,
) -> Result<PathBuf, String> {
    Ok(agent_directory(source, cwd, config_home)?.join(format!("{agent_type}.md")))
}

pub fn get_actual_agent_file_path(
    agent: &AgentDefinition,
    cwd: &Path,
    config_home: &Path,
) -> Result<PathBuf, String> {
    let source = source_from_definition(agent.source);
    if source == AgentFileSource::BuiltIn {
        return Ok(PathBuf::from("Built-in"));
    }
    let filename = agent.filename.as_deref().unwrap_or(&agent.agent_type);
    Ok(agent_directory(source, cwd, config_home)?.join(format!("{filename}.md")))
}

pub fn get_actual_relative_agent_file_path(
    agent: &AgentDefinition,
    cwd: &Path,
    config_home: &Path,
) -> String {
    match agent.source {
        AgentDefinitionSource::BuiltIn => "Built-in".to_string(),
        AgentDefinitionSource::Plugin => {
            format!("Plugin: {}", agent.plugin.as_deref().unwrap_or("Unknown"))
        }
        AgentDefinitionSource::FlagSettings => "CLI argument".to_string(),
        AgentDefinitionSource::ProjectSettings => PathBuf::from(".")
            .join(".nyan")
            .join("agents")
            .join(format!(
                "{}.md",
                agent.filename.as_deref().unwrap_or(&agent.agent_type)
            ))
            .display()
            .to_string(),
        _ => get_actual_agent_file_path(agent, cwd, config_home)
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
    }
}

fn write_and_sync(path: &Path, content: &str, create_new: bool) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create(true)
        .truncate(!create_new)
        .create_new(create_new);
    let mut file = options.open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            format!("Agent file already exists: {}", path.display())
        } else {
            error.to_string()
        }
    })?;
    file.write_all(content.as_bytes())
        .map_err(|error| error.to_string())?;
    file.sync_data().map_err(|error| error.to_string())
}

#[allow(clippy::too_many_arguments)]
pub fn save_agent_to_file(
    source: AgentFileSource,
    agent_type: &str,
    when_to_use: &str,
    tools: Option<&[String]>,
    system_prompt: &str,
    check_exists: bool,
    color: Option<&str>,
    model: Option<&str>,
    memory: Option<AgentMemoryScope>,
    cwd: &Path,
    config_home: &Path,
) -> Result<PathBuf, String> {
    let path = get_new_agent_file_path(source, agent_type, cwd, config_home)?;
    let content = format_agent_as_markdown(
        agent_type,
        when_to_use,
        tools,
        system_prompt,
        color,
        model,
        memory,
    );
    write_and_sync(&path, &content, check_exists)?;
    Ok(path)
}

#[allow(clippy::too_many_arguments)]
pub fn update_agent_file(
    agent: &AgentDefinition,
    when_to_use: &str,
    tools: Option<&[String]>,
    system_prompt: &str,
    color: Option<&str>,
    model: Option<&str>,
    memory: Option<AgentMemoryScope>,
    cwd: &Path,
    config_home: &Path,
) -> Result<PathBuf, String> {
    if agent.source == AgentDefinitionSource::BuiltIn {
        return Err("Cannot update built-in agents".to_string());
    }
    let path = get_actual_agent_file_path(agent, cwd, config_home)?;
    let content = format_agent_as_markdown(
        &agent.agent_type,
        when_to_use,
        tools,
        system_prompt,
        color,
        model,
        memory,
    );
    write_and_sync(&path, &content, false)?;
    Ok(path)
}

pub fn delete_agent_from_file(
    agent: &AgentDefinition,
    cwd: &Path,
    config_home: &Path,
) -> Result<(), String> {
    if agent.source == AgentDefinitionSource::BuiltIn {
        return Err("Cannot delete built-in agents".to_string());
    }
    let path = get_actual_agent_file_path(agent, cwd, config_home)?;
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_escaping_optional_lines_and_all_tools_match_official() {
        let tools = vec!["Read".to_string(), "Edit".to_string()];
        let text = format_agent_as_markdown(
            "reviewer",
            "say \"yes\"\\next\nline",
            Some(&tools),
            "Review carefully",
            Some("blue"),
            Some("@heavy:high"),
            Some(AgentMemoryScope::Project),
        );
        assert!(text.contains("description: \"say \\\"yes\\\"\\\\next\\\\nline\""));
        assert!(
            text.contains("\ntools: Read, Edit\nmodel: @heavy:high\ncolor: blue\nmemory: project")
        );
        assert!(!text.contains("effort:"));
        assert!(
            !format_agent_as_markdown("a", "desc", None, "prompt", None, None, None)
                .contains("tools:")
        );
    }

    /// Every writable tier lives under a `.nyan/agents` directory, the same
    /// one the loader reads — the managed tier included.
    #[test]
    fn writable_tiers_use_the_directories_the_loader_reads() {
        let _lock = crate::utils::test_env::TEST_ENV_LOCK.lock().unwrap();
        let managed =
            std::env::temp_dir().join(format!("nyan-agent-managed-{}", uuid::Uuid::new_v4()));
        let _managed =
            crate::utils::test_env::EnvVarGuard::set("CLAUDE_CODE_MANAGED_SETTINGS_PATH", &managed);
        let cwd = Path::new("/repo");
        let config = Path::new("/config");
        let path = |source| get_new_agent_file_path(source, "a", cwd, config).unwrap();

        assert_eq!(
            path(AgentFileSource::UserSettings),
            config.join("agents/a.md")
        );
        assert_eq!(
            path(AgentFileSource::ProjectSettings),
            cwd.join(".nyan/agents/a.md")
        );
        assert_eq!(
            path(AgentFileSource::PolicySettings),
            managed.join(".nyan/agents/a.md")
        );
    }

    #[test]
    fn save_create_new_flush_update_and_delete_use_actual_filename() {
        let root = std::env::temp_dir().join(format!("nyan-agent-files-{}", uuid::Uuid::new_v4()));
        let config = root.join("config");
        let path = save_agent_to_file(
            AgentFileSource::ProjectSettings,
            "reviewer",
            "review changes",
            None,
            "A sufficiently long prompt",
            true,
            None,
            None,
            None,
            &root,
            &config,
        )
        .unwrap();
        assert!(path.exists());
        assert!(
            save_agent_to_file(
                AgentFileSource::ProjectSettings,
                "reviewer",
                "review changes",
                None,
                "A sufficiently long prompt",
                true,
                None,
                None,
                None,
                &root,
                &config
            )
            .unwrap_err()
            .contains("already exists")
        );
        let mut agent =
            AgentDefinition::new("renamed", "review", AgentDefinitionSource::ProjectSettings);
        agent.filename = Some("reviewer".to_string());
        update_agent_file(
            &agent,
            "updated description",
            Some(&[]),
            "Updated sufficiently long prompt",
            None,
            None,
            None,
            &root,
            &config,
        )
        .unwrap();
        assert!(fs::read_to_string(&path).unwrap().contains("name: renamed"));
        delete_agent_from_file(&agent, &root, &config).unwrap();
        assert!(!path.exists());
        delete_agent_from_file(&agent, &root, &config).unwrap();
        let _ = fs::remove_dir_all(root);
    }
}
