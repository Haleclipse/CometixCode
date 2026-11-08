//! Maps to: CC `components/skills/SkillsMenu.tsx:1-205`.

use crate::components::configurable_shortcut_hint::ConfigurableShortcutHint;
use crate::components::design_system::dialog::Dialog;
use crate::skills::load_skills_dir::{
    SkillCommand, SkillLoadedFrom, SkillSource, SkillsPathSource,
    estimate_skill_frontmatter_tokens, get_skills_path,
};
use crate::utils::file::get_display_path;
use crate::utils::format::format_tokens;
use crate::utils::worktree::CommandResultDisplay;
use iocraft::prelude::*;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SkillMenuSource {
    UserSettings,
    ProjectSettings,
    LocalSettings,
    FlagSettings,
    Plugin,
    Bundled,
    Mcp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkillMenuLoadedFrom {
    Skills,
    CommandsDeprecated,
    Plugin,
    Bundled,
    Mcp,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillMenuCommand {
    pub name: String,
    pub display_name: Option<String>,
    pub description: String,
    pub when_to_use: Option<String>,
    pub source: SkillMenuSource,
    pub loaded_from: SkillMenuLoadedFrom,
    pub plugin_name: Option<String>,
}

impl SkillMenuCommand {
    pub fn command_name(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.name)
    }
}

impl From<&SkillCommand> for SkillMenuCommand {
    fn from(skill: &SkillCommand) -> Self {
        Self {
            name: skill.name.clone(),
            display_name: skill.display_name.clone(),
            description: skill.description.clone(),
            when_to_use: skill.when_to_use.clone(),
            source: match skill.source {
                SkillSource::UserSettings => SkillMenuSource::UserSettings,
                SkillSource::ProjectSettings => SkillMenuSource::ProjectSettings,
            },
            loaded_from: match skill.loaded_from {
                SkillLoadedFrom::Skills => SkillMenuLoadedFrom::Skills,
                SkillLoadedFrom::CommandsDeprecated => SkillMenuLoadedFrom::CommandsDeprecated,
                SkillLoadedFrom::Plugin => SkillMenuLoadedFrom::Plugin,
                SkillLoadedFrom::Bundled => SkillMenuLoadedFrom::Bundled,
                SkillLoadedFrom::Mcp => SkillMenuLoadedFrom::Mcp,
            },
            plugin_name: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillsMenuDone {
    pub result: String,
    pub display: CommandResultDisplay,
}

fn source_title(source: SkillMenuSource) -> &'static str {
    match source {
        SkillMenuSource::UserSettings => "User skills",
        SkillMenuSource::ProjectSettings => "Project skills",
        SkillMenuSource::LocalSettings => "Project, gitignored skills",
        SkillMenuSource::FlagSettings => "Cli flag skills",
        SkillMenuSource::Plugin => "Plugin skills",
        SkillMenuSource::Bundled => "Built-in skills",
        SkillMenuSource::Mcp => "MCP skills",
    }
}

/// Maps to: CC `SkillsMenu.tsx:70` `getDisplayPath(getSkillsPath(source, …))`.
///
/// `getSkillsPath` itself is the loader's, imported here exactly as CC imports
/// it from `loadSkillsDir.js`. Only the UI-enum → `SettingSource` projection is
/// local, plus the port-only `Bundled` group, which CC's switch has no arm for.
fn skills_path(source: SkillMenuSource, directory: &str) -> String {
    use crate::utils::settings::constants::SettingSource;

    let path = match source {
        SkillMenuSource::UserSettings => {
            get_skills_path(SkillsPathSource::Setting(SettingSource::User), directory)
        }
        SkillMenuSource::ProjectSettings => {
            get_skills_path(SkillsPathSource::Setting(SettingSource::Project), directory)
        }
        SkillMenuSource::LocalSettings => {
            get_skills_path(SkillsPathSource::Setting(SettingSource::Local), directory)
        }
        SkillMenuSource::FlagSettings => {
            get_skills_path(SkillsPathSource::Setting(SettingSource::Flag), directory)
        }
        SkillMenuSource::Plugin => get_skills_path(SkillsPathSource::Plugin, directory),
        SkillMenuSource::Bundled => PathBuf::from("bundled"),
        SkillMenuSource::Mcp => PathBuf::new(),
    };
    get_display_path(&path.display().to_string())
}

fn source_subtitle(source: SkillMenuSource, skills: &[SkillMenuCommand]) -> Option<String> {
    if source == SkillMenuSource::Mcp {
        let mut servers = Vec::<String>::new();
        for server in skills.iter().filter_map(|skill| {
            skill
                .name
                .split_once(':')
                .map(|(server, _)| server.to_string())
        }) {
            if !servers.contains(&server) {
                servers.push(server);
            }
        }
        return (!servers.is_empty()).then(|| servers.join(", "));
    }
    let path = skills_path(source, "skills");
    Some(
        if skills
            .iter()
            .any(|skill| skill.loaded_from == SkillMenuLoadedFrom::CommandsDeprecated)
        {
            format!("{path}, {}", skills_path(source, "commands"))
        } else {
            path
        },
    )
}

/// Maps to: CC `SkillsMenu.tsx:138` `estimateSkillFrontmatterTokens(skill)` —
/// the row-level call. The estimator itself belongs to the loader, which is
/// where CC exports it from (`loadSkillsDir.ts:100-105`).
fn estimate_row_frontmatter_tokens(skill: &SkillMenuCommand) -> u64 {
    estimate_skill_frontmatter_tokens(
        &skill.name,
        &skill.description,
        skill.when_to_use.as_deref(),
    )
    .max(0) as u64
}

fn grouped_skills(
    commands: &[SkillMenuCommand],
) -> BTreeMap<SkillMenuSource, Vec<SkillMenuCommand>> {
    let mut groups = BTreeMap::new();
    for command in commands {
        groups
            .entry(command.source)
            .or_insert_with(Vec::new)
            .push(command.clone());
    }
    for group in groups.values_mut() {
        group.sort_by(|a, b| a.command_name().cmp(b.command_name()));
    }
    groups
}

#[derive(Default, Props)]
pub struct SkillsMenuProps<'a> {
    pub commands: Vec<SkillMenuCommand>,
    pub on_exit: HandlerMut<'a, SkillsMenuDone>,
}

/// Maps to: CC `SkillsMenu`.
#[component]
pub fn SkillsMenu<'a>(
    props: &mut SkillsMenuProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let mut pending_done = hooks.use_state(|| Option::<SkillsMenuDone>::None);
    let done = { pending_done.read().clone() };
    if let Some(done) = done {
        pending_done.set(None);
        (props.on_exit)(done);
    }
    let skills = props.commands.clone();
    let groups = grouped_skills(&skills);
    let count = skills.len();
    let mut pending_done_for_cancel = pending_done;
    let cancel = move |_: ()| {
        pending_done_for_cancel.set(Some(SkillsMenuDone {
            result: "Skills dialog dismissed".to_string(),
            display: CommandResultDisplay::System,
        }));
    };

    if skills.is_empty() {
        return element! {
            Dialog(
                title: "Skills".to_string(),
                subtitle: "No skills found".to_string(),
                hide_input_guide: true,
                on_cancel: cancel,
            ) {
                Text(content: "Create skills in .nyan/skills/ or ~/.nyan/skills/".to_string(), dim: true)
                ConfigurableShortcutHint(
                    action: "confirm:no".to_string(), context: "Confirmation".to_string(),
                    fallback: "Esc".to_string(), description: "close".to_string(),
                    dim: true, italic: true,
                )
            }
        };
    }

    let order = [
        SkillMenuSource::ProjectSettings,
        SkillMenuSource::UserSettings,
        SkillMenuSource::Plugin,
        SkillMenuSource::Bundled,
        SkillMenuSource::Mcp,
    ];
    let rows = order.into_iter().filter_map(|source| {
        let group = groups.get(&source)?;
        let subtitle = source_subtitle(source, group);
        let items = group.iter().map(|skill| {
            let plugin = skill.plugin_name.as_ref().map(|name| format!(" · {name}")).unwrap_or_default();
            element! { View(flex_direction: FlexDirection::Row) {
                Text(content: skill.command_name().to_string())
                Text(content: format!("{plugin} · ~{} description tokens", format_tokens(estimate_row_frontmatter_tokens(skill))), dim: true)
            }}
        }).collect::<Vec<_>>();
        Some(element! { View(flex_direction: FlexDirection::Column) {
            View(flex_direction: FlexDirection::Row) {
                Text(content: source_title(source).to_string(), weight: Weight::Bold, dim: true)
                #(subtitle.map(|value| element! { Text(content: format!(" ({value})"), dim: true) }))
            }
            #(items)
        }})
    }).collect::<Vec<_>>();

    element! {
        Dialog(
            title: "Skills".to_string(),
            subtitle: format!("{count} {}", if count == 1 { "skill" } else { "skills" }),
            hide_input_guide: true,
            on_cancel: cancel,
        ) {
            View(flex_direction: FlexDirection::Column, gap: 1) { #(rows) }
            ConfigurableShortcutHint(
                action: "confirm:no".to_string(), context: "Confirmation".to_string(),
                fallback: "Esc".to_string(), description: "close".to_string(),
                dim: true, italic: true,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream::{self, StreamExt};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn skill(
        name: &str,
        source: SkillMenuSource,
        loaded_from: SkillMenuLoadedFrom,
    ) -> SkillMenuCommand {
        SkillMenuCommand {
            name: name.to_string(),
            display_name: None,
            description: "A useful description".to_string(),
            when_to_use: Some("Use for tests".to_string()),
            source,
            loaded_from,
            plugin_name: None,
        }
    }

    #[test]
    fn empty_menu_matches_official_copy_and_styles() {
        let theme = *crate::utils::theme::current();
        let canvas = element! {
            ContextProvider(value: Context::owned(theme)) {
                SkillsMenu(commands: Vec::new())
            }
        }
        .render(Some(100));
        let text = canvas.to_string();
        assert!(text.contains("Skills"));
        assert!(text.contains("No skills found"));
        assert!(text.contains("Create skills in .nyan/skills/ or ~/.nyan/skills/"));
        assert!(text.contains("Esc to close"));
        assert!(text.contains("\n\n  Esc to close"), "canvas=\n{text}");

        let (hint_y, hint_line) = text
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains("Esc to close"))
            .expect("hint line");
        let hint_x = hint_line.find("Esc").expect("hint column");
        let hint_style = canvas
            .resolved_text_style(hint_x, hint_y)
            .expect("hint style");
        assert!(hint_style.dim);
        assert!(hint_style.italic);

        let (title_y, title_line) = text
            .lines()
            .enumerate()
            .find(|(_, line)| line.trim() == "Skills")
            .expect("title line");
        let title_x = title_line.find("Skills").expect("title column");
        assert_eq!(
            canvas
                .resolved_text_style(title_x, title_y)
                .expect("title style")
                .color,
            Some(theme.permission)
        );
    }

    #[test]
    fn groups_sort_and_render_local_plugin_and_mcp_metadata() {
        let mut plugin = skill("zeta", SkillMenuSource::Plugin, SkillMenuLoadedFrom::Plugin);
        plugin.plugin_name = Some("demo-plugin".to_string());
        let commands = vec![
            skill(
                "zebra",
                SkillMenuSource::ProjectSettings,
                SkillMenuLoadedFrom::Skills,
            ),
            skill(
                "alpha",
                SkillMenuSource::ProjectSettings,
                SkillMenuLoadedFrom::CommandsDeprecated,
            ),
            plugin,
            skill(
                "server-b:two",
                SkillMenuSource::Mcp,
                SkillMenuLoadedFrom::Mcp,
            ),
            skill(
                "server-a:one",
                SkillMenuSource::Mcp,
                SkillMenuLoadedFrom::Mcp,
            ),
        ];
        let text = element! { SkillsMenu(commands) }
            .render(Some(160))
            .to_string();
        assert!(text.contains("5 skills"));
        assert!(text.contains("Project skills (.nyan/skills, .nyan/commands)"));
        assert!(text.find("alpha").unwrap() < text.find("zebra").unwrap());
        assert!(text.contains("zeta · demo-plugin · ~"));
        assert!(text.contains("MCP skills (server-a, server-b)"));
        assert!(text.contains("description tokens"));
    }

    #[test]
    fn escape_action_returns_official_system_dismissal() {
        let done = Arc::new(Mutex::new(Vec::<SkillsMenuDone>::new()));
        let done_for_handler = Arc::clone(&done);
        let done_for_wait = Arc::clone(&done);
        let events = stream::once(async {
            futures_timer::Delay::new(Duration::from_millis(35)).await;
            TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Esc))
        });

        futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                    ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                        SkillsMenu(
                            commands: Vec::new(),
                            on_exit: move |result| done_for_handler.lock().expect("done mutex").push(result),
                        )
                    }
                }
            };
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(events).with_size(100, 24),
            ));
            for _ in 0..20 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(80)).await;
                    None
                })
                .await;
                if next.is_none() || !done_for_wait.lock().expect("done mutex").is_empty() {
                    break;
                }
            }
        });

        assert_eq!(
            *done.lock().expect("done mutex"),
            vec![SkillsMenuDone {
                result: "Skills dialog dismissed".to_string(),
                display: CommandResultDisplay::System,
            }]
        );
    }

    #[test]
    fn token_estimate_uses_js_utf16_length_and_rounding() {
        let mut command = skill(
            "😀",
            SkillMenuSource::UserSettings,
            SkillMenuLoadedFrom::Skills,
        );
        command.description.clear();
        command.when_to_use = None;
        assert_eq!(estimate_row_frontmatter_tokens(&command), 1);
    }
}
