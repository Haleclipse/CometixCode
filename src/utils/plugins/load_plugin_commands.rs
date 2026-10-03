//! Maps to CC `utils/plugins/loadPluginCommands.ts`.
//! Plugin readers retain their own source metadata and execution-time closures;
//! the general skill DTO below is only the existing typed command representation.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures::FutureExt;
use indexmap::IndexMap;
use serde_json::Value;

use super::plugin_options_storage::{
    load_plugin_options, substitute_plugin_variables, substitute_user_config_in_content,
};
use super::schemas::PluginManifest;
use super::walk_plugin_markdown::{WalkPluginMarkdownOptions, walk_plugin_markdown};
use crate::commands::{Command, CommandSource, PluginInfo};
use crate::skills::load_skills_dir::{
    SkillCommand, SkillExecutionContext, SkillLoadedFrom, SkillSource,
};
use crate::types::plugin::LoadedPlugin;
use crate::utils::frontmatter_parser::{
    coerce_description_to_string, parse_boolean_frontmatter, parse_frontmatter,
    parse_shell_frontmatter,
};
use crate::utils::markdown_config_loader::extract_description_from_markdown;

/// Captured values of CC `createPluginCommand(...).getPromptForCommand`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCommandContext {
    pub plugin_manifest: Arc<PluginManifest>,
    pub plugin_path: PathBuf,
    pub source_name: String,
    pub is_skill_mode: bool,
}

/// Maps to CC `PluginMarkdownFile`.
#[derive(Clone)]
struct PluginMarkdownFile {
    file_path: PathBuf,
    base_dir: PathBuf,
    frontmatter: BTreeMap<String, Value>,
    content: String,
}

/// Maps to CC `LoadConfig`.
#[derive(Clone, Copy, Default)]
struct LoadConfig {
    is_skill_mode: bool,
}

type LoadedPaths = Arc<Mutex<HashSet<std::ffi::OsString>>>;

fn debug(message: impl AsRef<str>) {
    crate::utils::debug::log_for_debugging(message.as_ref());
}
fn error(message: impl AsRef<str>) {
    crate::utils::debug::log_for_debugging_with_level(
        message.as_ref(),
        crate::utils::debug::DebugLogLevel::Error,
    );
}

/// Maps to CC `isSkillFile`.
fn is_skill_file(file_path: &Path) -> bool {
    file_path
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("skill.md"))
}

/// Maps to CC `getCommandNameFromFile` (literal path-prefix namespace).
fn get_command_name_from_file(file_path: &Path, base_dir: &Path, plugin_name: &str) -> String {
    let directory = file_path.parent().unwrap_or(Path::new("."));
    let (namespace_dir, base_name) = if is_skill_file(file_path) {
        (
            directory.parent().unwrap_or(Path::new(".")),
            directory
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        )
    } else {
        let name = file_path.file_name().unwrap_or_default().to_string_lossy();
        (
            directory,
            name.strip_suffix(".md").unwrap_or(&name).to_string(),
        )
    };
    let dir = namespace_dir.to_string_lossy();
    let base = base_dir.to_string_lossy();
    let relative = dir.strip_prefix(base.as_ref()).unwrap_or("");
    let namespace = relative
        .strip_prefix('/')
        .unwrap_or(relative)
        .replace('/', ":");
    if namespace.is_empty() {
        format!("{plugin_name}:{base_name}")
    } else {
        format!("{plugin_name}:{namespace}:{base_name}")
    }
}

/// Maps to CC `collectMarkdownFiles`.
async fn collect_markdown_files(
    dir_path: &Path,
    base_dir: &Path,
    loaded_paths: LoadedPaths,
) -> Vec<PluginMarkdownFile> {
    let fs = crate::utils::fs_operations::get_fs_implementation();
    let files = Arc::new(Mutex::new(Vec::new()));
    let sink = files.clone();
    let base_dir = base_dir.to_path_buf();
    walk_plugin_markdown(
        dir_path,
        move |full_path, _namespace| {
            let files = sink.clone();
            let fs = fs.clone();
            let loaded_paths = loaded_paths.clone();
            let base_dir = base_dir.clone();
            async move {
                if crate::utils::fs_operations::is_duplicate_path(
                    fs.as_ref(),
                    &full_path,
                    &mut loaded_paths.lock().unwrap(),
                ) {
                    return Ok(());
                }
                let content = fs
                    .read_file(
                        &full_path,
                        crate::utils::fs_operations::BufferEncoding::Utf8,
                    )
                    .await
                    .map(|text| text.to_string_lossy())?;
                let parsed = parse_frontmatter(&content);
                files.lock().unwrap().push(PluginMarkdownFile {
                    file_path: full_path,
                    base_dir,
                    frontmatter: parsed.frontmatter,
                    content: parsed.content,
                });
                Ok(())
            }
        },
        WalkPluginMarkdownOptions {
            stop_at_skill_dir: true,
            log_label: Some("commands".into()),
        },
    )
    .await;
    // A failed onFile makes source Promise.all reject while siblings keep running.
    // Snapshot the entries completed at this boundary, retaining their completion order.
    let result = files.lock().unwrap().clone();
    result
}

/// Maps to CC `transformPluginSkillFiles`.
fn transform_plugin_skill_files(files: Vec<PluginMarkdownFile>) -> Vec<PluginMarkdownFile> {
    let mut by_dir: IndexMap<PathBuf, Vec<PluginMarkdownFile>> = IndexMap::new();
    for file in files {
        by_dir
            .entry(
                file.file_path
                    .parent()
                    .unwrap_or(Path::new("."))
                    .to_path_buf(),
            )
            .or_default()
            .push(file);
    }
    let mut result = Vec::new();
    for (dir, files) in by_dir {
        let mut skills = files.iter().filter(|file| is_skill_file(&file.file_path));
        if let Some(skill) = skills.next() {
            if skills.next().is_some() {
                debug(format!(
                    "Multiple skill files found in {}, using {}",
                    dir.display(),
                    skill
                        .file_path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                ));
            }
            result.push(skill.clone());
        } else {
            result.extend(files);
        }
    }
    result
}

/// Maps to CC `loadCommandsFromDirectory`.
async fn load_commands_from_directory(
    commands_path: &Path,
    plugin: &LoadedPlugin,
    config: LoadConfig,
    loaded_paths: LoadedPaths,
) -> Vec<Command> {
    let files = collect_markdown_files(commands_path, commands_path, loaded_paths).await;
    transform_plugin_skill_files(files)
        .into_iter()
        .filter_map(|file| {
            let name = get_command_name_from_file(&file.file_path, &file.base_dir, &plugin.name);
            let is_skill = is_skill_file(&file.file_path);
            create_plugin_command(name, file, plugin, is_skill, config)
        })
        .collect()
}

/// Maps to CC `createPluginCommand`.
fn create_plugin_command(
    command_name: String,
    file: PluginMarkdownFile,
    plugin: &LoadedPlugin,
    is_skill: bool,
    config: LoadConfig,
) -> Option<Command> {
    let frontmatter = &file.frontmatter;
    let description = coerce_description_to_string(frontmatter.get("description"));
    let mut raw_allowed = frontmatter.get("allowed-tools").cloned();
    match raw_allowed.as_mut() {
        Some(Value::String(value)) => {
            *value = match substitute_plugin_variables(value, &plugin.path, Some(&plugin.source)) {
                Ok(value) => value,
                Err(err) => {
                    error(format!(
                        "Failed to create command from {}: {err}",
                        file.file_path.display()
                    ));
                    return None;
                }
            }
        }
        Some(Value::Array(values)) => {
            for value in values {
                if let Value::String(value) = value {
                    *value = match substitute_plugin_variables(
                        value,
                        &plugin.path,
                        Some(&plugin.source),
                    ) {
                        Ok(value) => value,
                        Err(err) => {
                            error(format!(
                                "Failed to create command from {}: {err}",
                                file.file_path.display()
                            ));
                            return None;
                        }
                    };
                }
            }
        }
        _ => {}
    }
    // CC calls modelInput.trim() for a truthy model; malformed YAML values
    // throw into this function's catch instead of silently inheriting a model.
    if frontmatter.get("model").is_some_and(|value| match value {
        Value::Null | Value::Bool(false) | Value::String(_) => false,
        Value::Number(number) => number.as_f64().is_some_and(|number| number != 0.0),
        _ => true,
    }) {
        error(format!(
            "Failed to create command from {}: modelInput.trim is not a function",
            file.file_path.display()
        ));
        return None;
    }
    let field = |key: &str| {
        frontmatter
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let effort_raw = frontmatter.get("effort");
    let effort = if let Some(value) = effort_raw.filter(|value| !value.is_null()) {
        let raw = match crate::utils::json::JsoncValue::from_json(value.clone()).array_string() {
            Ok(raw) => raw,
            Err(()) => {
                error(format!(
                    "Failed to create command from {}: TypeError: No default value",
                    file.file_path.display()
                ));
                return None;
            }
        };
        crate::utils::effort::parse_effort_value(&raw)
    } else {
        None
    };
    if effort_raw.is_some() && effort.is_none() {
        debug(format!(
            "Plugin command {command_name} has invalid effort '{}'. Valid options: {} or an integer",
            effort_raw.unwrap(),
            crate::utils::effort::EFFORT_LEVELS.join(", ")
        ));
    }
    let skill = SkillCommand {
        name: command_name.clone(),
        display_name: field("name").filter(|name| !name.is_empty()),
        description: description.clone().unwrap_or_else(|| {
            extract_description_from_markdown(
                &file.content,
                if is_skill {
                    "Plugin skill"
                } else {
                    "Plugin command"
                },
            )
        }),
        has_user_specified_description: description.is_some(),
        content_length: file.content.encode_utf16().count(),
        markdown_content: file.content,
        allowed_tools:
            crate::utils::markdown_config_loader::parse_slash_command_tools_from_frontmatter(
                raw_allowed.as_ref(),
            ),
        argument_hint: field("argument-hint"),
        argument_names: crate::utils::argument_substitution::parse_argument_names(
            frontmatter.get("arguments"),
        ),
        when_to_use: field("when_to_use"),
        version: field("version"),
        model: field("model")
            .filter(|model| !model.is_empty() && model != "inherit")
            .map(|model| crate::utils::model::model::parse_user_specified_model(&model)),
        effort,
        disable_model_invocation: parse_boolean_frontmatter(
            frontmatter.get("disable-model-invocation"),
        ),
        user_invocable: frontmatter
            .get("user-invocable")
            .is_none_or(|value| parse_boolean_frontmatter(Some(value))),
        shell: parse_shell_frontmatter(frontmatter.get("shell"), &command_name),
        execution_context: SkillExecutionContext::Inline,
        agent: None,
        hooks: None,
        paths: None,
        source: SkillSource::ProjectSettings,
        loaded_from: SkillLoadedFrom::Plugin,
        skill_root: None,
        file_path: file.file_path,
    };
    let manifest = Arc::new(plugin.manifest.clone());
    let mut command = Command::from_skill(skill);
    command.source = CommandSource::Plugin;
    command.loaded_from = (is_skill || config.is_skill_mode).then_some(SkillLoadedFrom::Plugin);
    command.progress_message = Some(Cow::Borrowed(if is_skill || config.is_skill_mode {
        "loading"
    } else {
        "running"
    }));
    command.plugin_info = Some(PluginInfo {
        plugin_manifest: manifest.clone(),
        repository: plugin.source.clone(),
    });
    command.plugin_context = Some(Arc::new(PluginCommandContext {
        plugin_manifest: manifest,
        plugin_path: plugin.path.clone(),
        source_name: plugin.source.clone(),
        is_skill_mode: config.is_skill_mode,
    }));
    command.get_prompt_for_command = Some(get_prompt_for_command);
    Some(command)
}

// L1: the two source string-replacement regular expressions.
static SKILL_DIR_VARIABLE: std::sync::LazyLock<regress::Regex> = std::sync::LazyLock::new(|| {
    regress::Regex::new(r"\$\{CLAUDE_SKILL_DIR\}").expect("source regex")
});
static SESSION_ID_VARIABLE: std::sync::LazyLock<regress::Regex> = std::sync::LazyLock::new(|| {
    regress::Regex::new(r"\$\{CLAUDE_SESSION_ID\}").expect("source regex")
});

/// Maps to CC `createPluginCommand(...).getPromptForCommand`.
pub fn get_prompt_for_command(
    command: &Command,
    args: &str,
    context: &crate::tool::ToolUseContext,
) -> anyhow::Result<Vec<crate::types::message::UserContent>> {
    let skill = command
        .prompt_command
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Plugin command is missing prompt data"))?;
    let captured = command
        .plugin_context
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Plugin command is missing closure data"))?;
    let dir = skill.file_path.parent().unwrap_or(Path::new("."));
    let mut content = if captured.is_skill_mode {
        format!(
            "Base directory for this skill: {}\n\n{}",
            dir.display(),
            skill.markdown_content
        )
    } else {
        skill.markdown_content.clone()
    };
    content = crate::utils::argument_substitution::substitute_arguments(
        &content,
        Some(args),
        true,
        &skill.argument_names,
    )?;
    content =
        substitute_plugin_variables(&content, &captured.plugin_path, Some(&captured.source_name))
            .map_err(anyhow::Error::msg)?;
    if let Some(schema) = captured.plugin_manifest.user_config.as_ref() {
        content = substitute_user_config_in_content(
            &content,
            &load_plugin_options(&captured.source_name),
            schema,
        )
        .map_err(anyhow::Error::msg)?;
    }
    if captured.is_skill_mode {
        let dir = dir.to_string_lossy();
        let dir = if cfg!(windows) {
            dir.replace('\\', "/")
        } else {
            dir.into_owned()
        };
        content = crate::utils::argument_substitution::replace_all_with_string(
            &content,
            &SKILL_DIR_VARIABLE,
            &dir,
        );
    }
    content = crate::utils::argument_substitution::replace_all_with_string(
        &content,
        &SESSION_ID_VARIABLE,
        &crate::bootstrap::state::get_session_id(),
    );
    let parent = context.clone();
    let rules = crate::utils::permissions::permission_setup::parse_tool_list_from_cli(
        &skill.allowed_tools,
    )
    .iter()
    .map(|rule| {
        crate::utils::permissions::permission_rule_parser::permission_rule_value_from_string(rule)
    })
    .collect::<Vec<_>>();
    let get_app_state = crate::tool::GetAppStateCallback::new(move || {
        let state = parent.get_app_state().unwrap_or_else(|| {
            let mut state = crate::state::app_state_store::AppState::default();
            state.tool_permission_context = Arc::new(parent.tool_permission_context.clone());
            Arc::new(state)
        });
        let mut state = (*state).clone();
        let mut permissions = (*state.tool_permission_context).clone();
        permissions.always_allow_rules.insert(
            crate::types::permissions::PermissionRuleSource::Command,
            rules.clone(),
        );
        state.tool_permission_context = Arc::new(permissions);
        Some(Arc::new(state))
    });
    content = crate::utils::prompt_shell_execution::execute_shell_commands_in_prompt(
        &content,
        &context.clone().with_get_app_state_override(get_app_state),
        &format!("/{}", command.name),
        skill.shell,
    )?;
    Ok(vec![crate::types::message::UserContent::Text(content)])
}

// Source metadata's conditional object spreads. This is representation glue,
// not a separate command-loading algorithm; JS [] is truthy and clears tools.
fn metadata_frontmatter(
    mut frontmatter: BTreeMap<String, Value>,
    metadata: Option<&Value>,
) -> BTreeMap<String, Value> {
    if let Some(metadata) = metadata {
        for (key, field) in [
            ("description", "description"),
            ("argumentHint", "argument-hint"),
            ("model", "model"),
        ] {
            if let Some(value) = metadata
                .get(key)
                .filter(|value| value.as_str().is_some_and(|text| !text.is_empty()))
            {
                frontmatter.insert(field.into(), value.clone());
            }
        }
        if let Some(values) = metadata.get("allowedTools").and_then(Value::as_array) {
            frontmatter.insert(
                "allowed-tools".into(),
                Value::String(
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(","),
                ),
            );
        }
    }
    frontmatter
}

// Source commandsPaths.map body; each path catches its own failure.
async fn load_command_path(
    path: PathBuf,
    plugin: Arc<LoadedPlugin>,
    loaded_paths: LoadedPaths,
) -> Vec<Command> {
    let result: anyhow::Result<Vec<Command>> = async {
        let fs = crate::utils::fs_operations::get_fs_implementation();
        let stats = fs.stat(&path).await?;
        if stats.is_dir() {
            return Ok(load_commands_from_directory(
                &path,
                &plugin,
                LoadConfig::default(),
                loaded_paths,
            )
            .await);
        }
        if !stats.is_file() || !path.to_string_lossy().ends_with(".md") {
            return Ok(Vec::new());
        }
        if crate::utils::fs_operations::is_duplicate_path(
            fs.as_ref(),
            &path,
            &mut loaded_paths.lock().unwrap(),
        ) {
            return Ok(Vec::new());
        }
        let parsed = parse_frontmatter(
            &fs.read_file(&path, crate::utils::fs_operations::BufferEncoding::Utf8)
                .await
                .map(|text| text.to_string_lossy())?,
        );
        let matching = plugin
            .commands_metadata
            .as_ref()
            .and_then(Value::as_object)
            .and_then(|entries| {
                entries.iter().find(|(_, metadata)| {
                    metadata
                        .get("source")
                        .and_then(Value::as_str)
                        .is_some_and(|source| {
                            super::plugin_loader::node_path_join(&plugin.path, source).as_os_str()
                                == path.as_os_str()
                        })
                })
            });
        let name = matching.map(|(name, _)| name.clone()).unwrap_or_else(|| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .strip_suffix(".md")
                .unwrap_or_default()
                .to_string()
        });
        let file = PluginMarkdownFile {
            file_path: path.clone(),
            base_dir: path.parent().unwrap_or(Path::new(".")).to_path_buf(),
            frontmatter: metadata_frontmatter(
                parsed.frontmatter,
                matching.map(|(_, metadata)| metadata),
            ),
            content: parsed.content,
        };
        Ok(create_plugin_command(
            format!("{}:{name}", plugin.name),
            file,
            &plugin,
            false,
            LoadConfig::default(),
        )
        .into_iter()
        .collect())
    }
    .await;
    match result {
        Ok(commands) => commands,
        Err(err) => {
            error(format!(
                "Failed to load commands from plugin {} custom path {}: {err}",
                plugin.name,
                path.display()
            ));
            Vec::new()
        }
    }
}

/// Maps to CC `loadSkillsFromDirectory`.
async fn load_skills_from_directory(
    skills_path: &Path,
    plugin: &LoadedPlugin,
    loaded_paths: LoadedPaths,
) -> Vec<Command> {
    let fs = crate::utils::fs_operations::get_fs_implementation();
    let direct_path = skills_path.join("SKILL.md");
    match fs
        .read_file(
            &direct_path,
            crate::utils::fs_operations::BufferEncoding::Utf8,
        )
        .await
        .map(|text| text.to_string_lossy())
    {
        Ok(content) => {
            if crate::utils::fs_operations::is_duplicate_path(
                fs.as_ref(),
                &direct_path,
                &mut loaded_paths.lock().unwrap(),
            ) {
                return Vec::new();
            }
            let parsed = parse_frontmatter(&content);
            let file = PluginMarkdownFile {
                file_path: direct_path,
                base_dir: skills_path.to_path_buf(),
                frontmatter: parsed.frontmatter,
                content: parsed.content,
            };
            return create_plugin_command(
                format!(
                    "{}:{}",
                    plugin.name,
                    skills_path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                ),
                file,
                plugin,
                true,
                LoadConfig {
                    is_skill_mode: true,
                },
            )
            .into_iter()
            .collect();
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            error(format!(
                "Failed to load skill from {}: {err}",
                direct_path.display()
            ));
            return Vec::new();
        }
    }
    let entries = match fs.readdir(skills_path).await {
        Ok(entries) => entries,
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                error(format!(
                    "Failed to load skills from directory {}: {err}",
                    skills_path.display()
                ));
            }
            return Vec::new();
        }
    };
    let skills = Arc::new(Mutex::new(Vec::new()));
    let mut workers = Vec::new();
    let runtime = crate::utils::process_runtime::runtime_handle_for_detached_work()
        .expect("plugin loading requires process lifetime runtime");
    for entry in entries {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_dir() && !kind.is_symlink() {
            continue;
        }
        let path = entry.path().join("SKILL.md");
        let name = format!("{}:{}", plugin.name, entry.file_name().to_string_lossy());
        let loaded_paths = loaded_paths.clone();
        let skills = skills.clone();
        let plugin = plugin.clone();
        let fs = fs.clone();
        workers.push(runtime.spawn(async move {
            let content = match fs
                .read_file(&path, crate::utils::fs_operations::BufferEncoding::Utf8)
                .await
                .map(|text| text.to_string_lossy())
            {
                Ok(content) => content,
                Err(err) => {
                    if err.kind() != std::io::ErrorKind::NotFound {
                        error(format!(
                            "Failed to load skill from {}: {err}",
                            path.display()
                        ));
                    }
                    return;
                }
            };
            if crate::utils::fs_operations::is_duplicate_path(
                fs.as_ref(),
                &path,
                &mut loaded_paths.lock().unwrap(),
            ) {
                return;
            }
            let parsed = parse_frontmatter(&content);
            let file = PluginMarkdownFile {
                base_dir: path.parent().unwrap_or(Path::new(".")).to_path_buf(),
                file_path: path,
                frontmatter: parsed.frontmatter,
                content: parsed.content,
            };
            if let Some(command) = create_plugin_command(
                name,
                file,
                &plugin,
                true,
                LoadConfig {
                    is_skill_mode: true,
                },
            ) {
                skills.lock().unwrap().push(command);
            }
        }));
    }
    let _ = futures::future::try_join_all(workers).await;
    let result = skills.lock().unwrap().clone();
    result
}

// The two source memoized async functions have independent promise identities.
type CommandPromise = futures::future::Shared<
    futures::future::BoxFuture<'static, Result<Arc<Vec<Command>>, Arc<anyhow::Error>>>,
>;
static COMMAND_CACHE: Mutex<Option<CommandPromise>> = Mutex::new(None);
static SKILL_CACHE: Mutex<Option<CommandPromise>> = Mutex::new(None);

fn memoized_commands(
    cache: &'static Mutex<Option<CommandPromise>>,
    skill_mode: bool,
) -> impl std::future::Future<Output = anyhow::Result<Arc<Vec<Command>>>> + Send {
    let promise = {
        let mut cache = cache.lock().unwrap();
        cache
            .get_or_insert_with(|| {
                let promise = load_plugin_catalog(skill_mode).boxed().shared();
                let worker = promise.clone();
                crate::utils::process_runtime::runtime_handle_for_detached_work()
                    .expect("plugin loading requires process lifetime runtime")
                    .spawn(async move {
                        let _ = worker.await;
                    });
                promise
            })
            .clone()
    };
    async move { promise.await.map_err(|err| anyhow::anyhow!("{err}")) }
}

/// Maps to CC `getPluginCommands`.
pub fn get_plugin_commands()
-> impl std::future::Future<Output = anyhow::Result<Arc<Vec<Command>>>> + Send {
    memoized_commands(&COMMAND_CACHE, false)
}
/// Maps to CC `getPluginSkills`.
pub fn get_plugin_skills()
-> impl std::future::Future<Output = anyhow::Result<Arc<Vec<Command>>>> + Send {
    memoized_commands(&SKILL_CACHE, true)
}
/// Maps to CC `clearPluginCommandCache`.
pub fn clear_plugin_command_cache() {
    *COMMAND_CACHE.lock().unwrap() = None;
}
/// Maps to CC `clearPluginSkillsCache`.
pub fn clear_plugin_skills_cache() {
    *SKILL_CACHE.lock().unwrap() = None;
}

// Rust async carrier for the two source memoize callbacks. Their per-plugin
// loadedPaths and source ordering remain distinct from the outer command cache.
async fn load_plugin_catalog(skill_mode: bool) -> Result<Arc<Vec<Command>>, Arc<anyhow::Error>> {
    if crate::utils::env_utils::is_bare_mode()
        && crate::bootstrap::state::get_inline_plugins().is_empty()
    {
        return Ok(Arc::new(Vec::new()));
    }
    let loaded = super::plugin_loader::load_all_plugins_cache_only()
        .await
        .map_err(Arc::new)?;
    let errors = loaded.errors.snapshot();
    if !errors.is_empty() {
        debug(format!(
            "Plugin loading errors: {}",
            errors
                .iter()
                .map(crate::types::plugin::get_plugin_error_message)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let runtime = crate::utils::process_runtime::runtime_handle_for_detached_work()
        .expect("plugin loading requires process lifetime runtime");
    let workers = loaded
        .enabled
        .into_iter()
        .map(|plugin| {
            runtime.spawn(async move {
                let loaded_paths = Arc::new(Mutex::new(HashSet::new()));
                let plugin = Arc::new(plugin);
                let mut commands = Vec::new();
                if skill_mode {
                    if let Some(path) = plugin.skills_path.as_ref() {
                        commands.extend(
                            load_skills_from_directory(path, &plugin, loaded_paths.clone()).await,
                        );
                    }
                    let tasks = plugin
                        .skills_paths
                        .iter()
                        .map(|path| {
                            let path = path.clone();
                            let plugin = plugin.clone();
                            let loaded_paths = loaded_paths.clone();
                            crate::utils::process_runtime::runtime_handle_for_detached_work()
                                .unwrap()
                                .spawn(async move {
                                    load_skills_from_directory(&path, &plugin, loaded_paths).await
                                })
                        })
                        .collect::<Vec<_>>();
                    for task in futures::future::try_join_all(tasks).await? {
                        commands.extend(task);
                    }
                } else {
                    if let Some(path) = plugin.commands_path.as_ref() {
                        commands.extend(
                            load_commands_from_directory(
                                path,
                                &plugin,
                                LoadConfig::default(),
                                loaded_paths.clone(),
                            )
                            .await,
                        );
                    }
                    let tasks = plugin
                        .commands_paths
                        .iter()
                        .map(|path| {
                            crate::utils::process_runtime::runtime_handle_for_detached_work()
                                .unwrap()
                                .spawn(load_command_path(
                                    path.clone(),
                                    plugin.clone(),
                                    loaded_paths.clone(),
                                ))
                        })
                        .collect::<Vec<_>>();
                    for task in futures::future::try_join_all(tasks).await? {
                        commands.extend(task);
                    }
                    if let Some(metadata) =
                        plugin.commands_metadata.as_ref().and_then(Value::as_object)
                    {
                        for (name, metadata) in metadata {
                            let Some(content) = metadata
                                .get("content")
                                .and_then(Value::as_str)
                                .filter(|content| !content.is_empty())
                            else {
                                continue;
                            };
                            if metadata
                                .get("source")
                                .and_then(Value::as_str)
                                .is_some_and(|source| !source.is_empty())
                            {
                                continue;
                            }
                            let parsed = parse_frontmatter(content);
                            let name = format!("{}:{name}", plugin.name);
                            let file = PluginMarkdownFile {
                                file_path: PathBuf::from(format!("<inline:{name}>")),
                                base_dir: plugin.path.clone(),
                                frontmatter: metadata_frontmatter(
                                    parsed.frontmatter,
                                    Some(metadata),
                                ),
                                content: parsed.content,
                            };
                            if let Some(command) = create_plugin_command(
                                name,
                                file,
                                &plugin,
                                false,
                                LoadConfig::default(),
                            ) {
                                commands.push(command);
                            }
                        }
                    }
                }
                Ok::<_, anyhow::Error>(commands)
            })
        })
        .collect::<Vec<_>>();
    let grouped = futures::future::try_join_all(workers)
        .await
        .map_err(|err| Arc::new(err.into()))?;
    let mut commands = Vec::new();
    for group in grouped {
        commands.extend(group.map_err(Arc::new)?);
    }
    debug(format!(
        "Total plugin {} loaded: {}",
        if skill_mode { "skills" } else { "commands" },
        commands.len()
    ));
    Ok(Arc::new(commands))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("plugin-commands-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
        fn write(&self, path: &str, body: &str) -> PathBuf {
            let path = self.0.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, body).unwrap();
            path
        }
        fn plugin(&self) -> LoadedPlugin {
            LoadedPlugin {
                name: "p".into(),
                source: "p@inline".into(),
                path: self.0.clone(),
                manifest: PluginManifest {
                    name: "p".into(),
                    ..Default::default()
                },
                enabled: true,
                ..Default::default()
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn paths() -> LoadedPaths {
        Arc::new(Mutex::new(HashSet::new()))
    }
    fn names(commands: &[Command]) -> Vec<String> {
        commands
            .iter()
            .map(|command| command.name.to_string())
            .collect()
    }
    fn file(path: impl Into<PathBuf>, body: &str) -> PluginMarkdownFile {
        let path = path.into();
        let parsed = parse_frontmatter(body);
        PluginMarkdownFile {
            base_dir: path.parent().unwrap_or(Path::new(".")).to_path_buf(),
            file_path: path,
            frontmatter: parsed.frontmatter,
            content: parsed.content,
        }
    }
    fn text(command: &Command, args: &str) -> String {
        let output =
            get_prompt_for_command(command, args, &crate::tool::ToolUseContext::default()).unwrap();
        match &output[0] {
            crate::types::message::UserContent::Text(text) => text.clone(),
            _ => panic!("source returns text"),
        }
    }

    #[test]
    fn plugin_names_match_actual_bun_oracle() {
        let oracle: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/oracles/plugin-commands-0916/source-oracle.json"
        ))
        .unwrap();
        for case in oracle["naming"].as_array().unwrap() {
            assert_eq!(
                get_command_name_from_file(
                    Path::new(case["file"].as_str().unwrap()),
                    Path::new(case["base"].as_str().unwrap()),
                    "p"
                ),
                case["name"].as_str().unwrap()
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn command_paths_metadata_and_skill_leaf_share_only_per_reader_dedup() {
        crate::utils::process_runtime::initialize_test_process_runtime();
        let fixture = Fixture::new();
        fixture.write("commands/nested/leaf/SKILL.md", "Leaf");
        fixture.write("commands/nested/leaf/notes.md", "Ignored sibling");
        fixture.write("commands/nested/leaf/deeper/ignored.md", "Ignored child");
        let default = fixture.write("commands/default.md", "Default");
        let custom = fixture.write(
            "custom/one.md",
            "---\nallowed-tools: Bash\ndescription: Frontmatter\n---\nCustom",
        );
        let upper = fixture.write("custom/UP.MD", "Upper");
        let mut plugin = fixture.plugin();
        plugin.commands_metadata = Some(serde_json::json!({
            "replacement": {"source":"commands/default.md", "description":"Must not replace default"},
            "alias": {"source":"custom/one.md", "description":"Override", "allowedTools":[]}
        }));
        let loaded_paths = paths();
        let defaults = load_commands_from_directory(
            &fixture.0.join("commands"),
            &plugin,
            LoadConfig::default(),
            loaded_paths.clone(),
        )
        .await;
        let mut default_names = names(&defaults);
        default_names.sort();
        assert_eq!(default_names, ["p:default", "p:nested:leaf"]);
        let plugin = Arc::new(plugin);
        assert!(
            load_command_path(default, plugin.clone(), loaded_paths.clone())
                .await
                .is_empty()
        );
        let custom_commands = load_command_path(custom, plugin.clone(), loaded_paths.clone()).await;
        assert_eq!(names(&custom_commands), ["p:alias"]);
        assert_eq!(custom_commands[0].description, "Override");
        assert!(custom_commands[0].allowed_tools.is_empty());
        assert!(
            load_command_path(upper, plugin.clone(), loaded_paths.clone())
                .await
                .is_empty()
        );
        let direct_skills =
            load_skills_from_directory(&fixture.0.join("commands/nested/leaf"), &plugin, paths())
                .await;
        assert_eq!(names(&direct_skills), ["p:leaf"]);
        assert!(text(&direct_skills[0], "").starts_with("Base directory for this skill:"));
        let command_leaf = defaults.iter().find(|c| c.name == "p:nested:leaf").unwrap();
        assert!(!text(command_leaf, "").starts_with("Base directory for this skill:"));
        assert_eq!(command_leaf.progress_message.as_deref(), Some("loading"));
    }

    #[test]
    fn plugin_prompt_captures_options_at_execution_and_only_skill_mode_expands_skill_dir() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let fixture = Fixture::new();
        let config = fixture.0.join("config");
        std::fs::create_dir_all(&config).unwrap();
        let _env = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config);
        let previous_session = crate::bootstrap::state::get_session_id();
        struct Restore(String);
        impl Drop for Restore {
            fn drop(&mut self) {
                crate::bootstrap::state::set_session_id(self.0.clone());
                super::super::plugin_options_storage::clear_plugin_options_cache();
            }
        }
        let _restore = Restore(previous_session);
        let mut plugin = fixture.plugin();
        plugin.manifest.user_config = Some(serde_json::json!({"label":{"type":"string"}}));
        let body = "$ARGUMENTS ${user_config.label} ${CLAUDE_SKILL_DIR} ${CLAUDE_SESSION_ID} 😀";
        let mut input = file(fixture.0.join("commands/leaf/SKILL.md"), body);
        input
            .frontmatter
            .insert("context".into(), Value::String("fork".into()));
        input
            .frontmatter
            .insert("agent".into(), Value::String("never".into()));
        input
            .frontmatter
            .insert("hooks".into(), serde_json::json!({"Stop":[]}));
        input
            .frontmatter
            .insert("paths".into(), serde_json::json!(["never"]));
        let command =
            create_plugin_command("p:leaf".into(), input, &plugin, true, LoadConfig::default())
                .unwrap();
        let skill = create_plugin_command(
            "p:leaf".into(),
            file(fixture.0.join("skills/leaf/SKILL.md"), body),
            &plugin,
            true,
            LoadConfig {
                is_skill_mode: true,
            },
        )
        .unwrap();
        let schema = plugin.manifest.user_config.as_ref().unwrap().clone();
        super::super::plugin_options_storage::save_plugin_options(
            &plugin.source,
            &serde_json::json!({"label":"second"})
                .as_object()
                .unwrap()
                .clone(),
            &schema,
        )
        .unwrap();
        crate::bootstrap::state::set_session_id("S2");
        let expected = format!("{} second ${{CLAUDE_SKILL_DIR}} S2 😀", fixture.0.display());
        assert_eq!(text(&command, "${CLAUDE_PLUGIN_ROOT}"), expected);
        assert_eq!(
            text(&skill, "${CLAUDE_PLUGIN_ROOT}"),
            format!(
                "Base directory for this skill: {0}/skills/leaf\n\n{0} second {0}/skills/leaf S2 😀",
                fixture.0.display()
            )
        );
        assert_eq!(command.content_length, Some(75));
        let dto = command.prompt_command.as_ref().unwrap();
        assert_eq!(dto.execution_context, SkillExecutionContext::Inline);
        assert!(dto.agent.is_none() && dto.hooks.is_none() && dto.paths.is_none());
        assert_eq!(command.plugin_info.as_ref().unwrap().repository, "p@inline");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn skill_symlinks_deduplicate_by_target_but_not_across_plugins() {
        crate::utils::process_runtime::initialize_test_process_runtime();
        let fixture = Fixture::new();
        fixture.write("skills/original/SKILL.md", "Body");
        std::os::unix::fs::symlink(
            fixture.0.join("skills/original"),
            fixture.0.join("skills/alias"),
        )
        .unwrap();
        let plugin = fixture.plugin();
        let found = load_skills_from_directory(&fixture.0.join("skills"), &plugin, paths()).await;
        assert_eq!(found.len(), 1);
        let mut other = plugin.clone();
        other.name = "other".into();
        let second = load_skills_from_directory(&fixture.0.join("skills"), &other, paths()).await;
        assert_eq!(second.len(), 1);
        assert!(second[0].name.starts_with("other:"));
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn plugin_catalog_loads_inline_content_custom_paths_and_independent_memos() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        crate::utils::process_runtime::initialize_test_process_runtime();
        let fixture = Fixture::new();
        let config = fixture.0.join("config");
        std::fs::create_dir_all(&config).unwrap();
        let _env = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config);
        let _bare = EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "1");
        let original = crate::bootstrap::state::get_inline_plugins();
        struct Restore(Vec<PathBuf>);
        impl Drop for Restore {
            fn drop(&mut self) {
                crate::bootstrap::state::set_inline_plugins(self.0.clone());
                clear_plugin_command_cache();
                clear_plugin_skills_cache();
                super::super::plugin_loader::clear_plugin_cache(None);
            }
        }
        let _restore = Restore(original);
        crate::bootstrap::state::set_inline_plugins(Vec::new());
        clear_plugin_command_cache();
        clear_plugin_skills_cache();
        assert!(get_plugin_commands().await.unwrap().is_empty());
        fixture.write(
            ".claude-plugin/plugin.json",
            &serde_json::json!({
                "name":"p", "commands": {
                    "custom": {"source":"./custom/source.md", "description":"Custom metadata"},
                    "inline": {"content":"Inline body", "argumentHint":"ARG"}
                }, "skills":["./direct"]
            })
            .to_string(),
        );
        fixture.write("custom/source.md", "Custom body");
        fixture.write("direct/SKILL.md", "Direct body");
        crate::bootstrap::state::set_inline_plugins(vec![fixture.0.clone()]);
        super::super::plugin_loader::clear_plugin_cache(None);
        clear_plugin_command_cache();
        clear_plugin_skills_cache();
        let commands = get_plugin_commands().await.unwrap();
        assert_eq!(names(&commands), ["p:custom", "p:inline"]);
        assert_eq!(commands[0].description, "Custom metadata");
        assert_eq!(commands[1].argument_hint.as_deref(), Some("ARG"));
        assert_eq!(text(&commands[1], ""), "Inline body");
        assert!(Arc::ptr_eq(
            &commands,
            &get_plugin_commands().await.unwrap()
        ));
        let skills = get_plugin_skills().await.unwrap();
        assert_eq!(names(&skills), ["p:direct"]);
        clear_plugin_skills_cache();
        assert!(Arc::ptr_eq(
            &commands,
            &get_plugin_commands().await.unwrap()
        ));
        assert!(!Arc::ptr_eq(&skills, &get_plugin_skills().await.unwrap()));
        clear_plugin_command_cache();
        assert!(!Arc::ptr_eq(
            &commands,
            &get_plugin_commands().await.unwrap()
        ));
    }
    #[test]
    fn skill_directory_and_session_use_source_string_replacement_tokens() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let original = crate::bootstrap::state::get_session_id();
        struct Restore(String);
        impl Drop for Restore {
            fn drop(&mut self) {
                crate::bootstrap::state::set_session_id(self.0.clone());
            }
        }
        let _restore = Restore(original);
        crate::bootstrap::state::set_session_id("S$&");
        let plugin = LoadedPlugin {
            name: "p".into(),
            source: "p@inline".into(),
            path: PathBuf::from("/p"),
            manifest: PluginManifest {
                name: "p".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let command = create_plugin_command(
            "p:dollar".into(),
            file(
                "/p/skills/$&-$$/SKILL.md",
                "${CLAUDE_SKILL_DIR}|${CLAUDE_SESSION_ID}",
            ),
            &plugin,
            true,
            LoadConfig {
                is_skill_mode: true,
            },
        )
        .unwrap();
        let oracle: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/oracles/plugin-commands-0916/replacement-oracle.json"
        ))
        .unwrap();
        assert_eq!(text(&command, ""), oracle[0]["text"].as_str().unwrap());
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn malformed_utf8_is_decoded_instead_of_dropping_plugin_commands() {
        crate::utils::process_runtime::initialize_test_process_runtime();
        let fixture = Fixture::new();
        let path = fixture.write("commands/value.md", "");
        std::fs::write(&path, b"hello \xff world").unwrap();
        let plugin = fixture.plugin();
        let commands = load_commands_from_directory(
            &fixture.0.join("commands"),
            &plugin,
            LoadConfig::default(),
            paths(),
        )
        .await;
        assert_eq!(commands.len(), 1);
        assert_eq!(text(&commands[0], ""), "hello \u{fffd} world");
        let direct = fixture.write("direct/SKILL.md", "");
        std::fs::write(&direct, b"skill \xff body").unwrap();
        let skills = load_skills_from_directory(&fixture.0.join("direct"), &plugin, paths()).await;
        assert_eq!(skills.len(), 1);
        assert!(text(&skills[0], "").ends_with("skill \u{fffd} body"));
    }

    #[test]
    fn plugin_effort_delegates_unknown_coercion_without_panicking() {
        let fixture = Fixture::new();
        let plugin = fixture.plugin();
        for (raw, expected) in [
            (
                serde_json::json!(["high"]),
                Some(crate::utils::effort::EffortValue::Named("high".into())),
            ),
            (
                serde_json::json!([5]),
                Some(crate::utils::effort::EffortValue::Numeric(5)),
            ),
            (serde_json::json!([null, 2]), None),
        ] {
            let mut input = file("/p/value.md", "body");
            input.frontmatter.insert("effort".into(), raw);
            let command = create_plugin_command(
                "p:value".into(),
                input,
                &plugin,
                false,
                LoadConfig::default(),
            )
            .unwrap();
            assert_eq!(command.prompt_command.as_ref().unwrap().effort, expected);
        }
        let mut input = file("/p/value.md", "body");
        input
            .frontmatter
            .insert("effort".into(), serde_json::json!({"toString":0}));
        assert!(
            create_plugin_command(
                "p:value".into(),
                input,
                &plugin,
                false,
                LoadConfig::default()
            )
            .is_none()
        );
    }
}
