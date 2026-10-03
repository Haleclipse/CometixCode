//! Maps to: CC `components/memory/MemoryFileSelector.tsx:1-324`.
//!
//! The selector owns file/folder options, toggle focus, settings writes, and
//! local folder opening. Imported/nested CLAUDE.md metadata and live dream-task
//! state remain explicit upstream-service seams because Rust's current
//! `ClaudeMdFile`/`AppState` projections do not expose those fields yet.

use crate::components::custom_select::{Select, SelectLayout, SelectOptionData};
use crate::components::design_system::list_item::ListItem;
use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::use_keybinding;
use crate::tools::agent_tool::agent_memory::get_agent_memory_dir;
use crate::tools::agent_tool::load_agents_dir::get_agent_definitions_with_overrides;
use crate::utils::claudemd::{
    ClaudeMdFile, ClaudeMdKind, ClaudeMdSource, discover_claude_md_files,
};
use crate::utils::format::format_relative_time_ago_millis;
use crate::utils::settings::{SettingSource, get_initial_settings, update_settings_for_source};
use crate::utils::{browser::open_path, file::get_display_path};
use iocraft::prelude::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const VISIBLE_MEMORY_OPTION_COUNT: usize = 5;
pub const OPEN_FOLDER_PREFIX: &str = "__open_folder__";

static LAST_SELECTED_PATH: LazyLock<Mutex<Option<String>>> = LazyLock::new(|| Mutex::new(None));

/// Live AppState task projection for the official dream-running status.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryDreamTaskRunning(pub bool);

#[derive(Clone, Debug, PartialEq, Eq)]
enum MemorySelectorAction {
    Cancel,
    Select(String),
    OpenFolder(PathBuf),
    ToggleAutoMemory,
    ToggleAutoDream,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MemoryFocus {
    AutoMemory,
    AutoDream,
    Select,
}

fn project_is_in_git_repo(cwd: &Path) -> bool {
    cwd.ancestors().any(|dir| dir.join(".git").exists())
}

fn default_user_memory_path(config_home: &Path) -> PathBuf {
    config_home.join("CLAUDE.md")
}

fn default_project_memory_path(cwd: &Path) -> PathBuf {
    cwd.join("CLAUDE.md")
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AutoMemorySnapshot {
    enabled: bool,
    dream_enabled: bool,
    path: Option<PathBuf>,
}

/// The initial toggle values of CC `MemoryFileSelector.tsx:206-212`
/// (`useState(isAutoMemoryEnabled)` and the dream toggle). The folder row of
/// `:166-170`, which CC reads on every render, is `resolved_auto_memory_path`
/// gated on the live toggle in the component.
fn auto_memory_snapshot() -> AutoMemorySnapshot {
    let enabled = crate::memdir::paths::is_auto_memory_enabled();
    AutoMemorySnapshot {
        enabled,
        dream_enabled: enabled && get_initial_settings().auto_dream_enabled.unwrap_or(false),
        path: enabled.then(crate::memdir::paths::get_auto_mem_path),
    }
}

fn memory_option_description(
    source: ClaudeMdSource,
    path: &Path,
    user_memory_path: &Path,
    project_memory_path: &Path,
    cwd: &Path,
    parent: Option<&Path>,
    is_nested: bool,
) -> String {
    if source == ClaudeMdSource::UserGlobal && !is_nested && path == user_memory_path {
        "Saved in ~/.claude/CLAUDE.md".to_string()
    } else if source == ClaudeMdSource::Project && !is_nested && path == project_memory_path {
        if project_is_in_git_repo(cwd) {
            "Checked in at ./CLAUDE.md".to_string()
        } else {
            "Saved in ./CLAUDE.md".to_string()
        }
    } else if parent.is_some() {
        "@-imported".to_string()
    } else if is_nested {
        "dynamically loaded".to_string()
    } else {
        String::new()
    }
}

fn memory_option_label(
    source: ClaudeMdSource,
    path: &Path,
    user_memory_path: &Path,
    project_memory_path: &Path,
    exists: bool,
    is_nested: bool,
    depth: usize,
) -> String {
    if source == ClaudeMdSource::UserGlobal && !is_nested && path == user_memory_path {
        "User memory".to_string()
    } else if source == ClaudeMdSource::Project && !is_nested && path == project_memory_path {
        "Project memory".to_string()
    } else {
        let exists_label = if exists { "" } else { " (new)" };
        let display = get_display_path(&path.display().to_string());
        if depth > 0 {
            format!("{}L {display}{exists_label}", "  ".repeat(depth - 1))
        } else {
            format!("{display}{exists_label}")
        }
    }
}

fn memory_select_options(
    cwd: &Path,
    config_home: &Path,
    existing_files: &[ClaudeMdFile],
    auto_memory_path: Option<&Path>,
) -> Vec<SelectOptionData> {
    let user_memory_path = default_user_memory_path(config_home);
    let project_memory_path = default_project_memory_path(cwd);
    let selectable_files = existing_files
        .iter()
        .filter(|file| !matches!(file.kind, ClaudeMdKind::AutoMem | ClaudeMdKind::TeamMem))
        .collect::<Vec<_>>();
    let mut seen = selectable_files
        .iter()
        .map(|file| file.path.clone())
        .collect::<HashSet<_>>();
    let mut entries = selectable_files
        .into_iter()
        .map(|file| {
            (
                file.path.clone(),
                file.source,
                true,
                file.parent.clone(),
                file.is_nested,
            )
        })
        .collect::<Vec<_>>();

    if seen.insert(user_memory_path.clone()) {
        entries.push((
            user_memory_path.clone(),
            ClaudeMdSource::UserGlobal,
            false,
            None,
            false,
        ));
    }
    if seen.insert(project_memory_path.clone()) {
        entries.push((
            project_memory_path.clone(),
            ClaudeMdSource::Project,
            false,
            None,
            false,
        ));
    }

    let mut depths = std::collections::HashMap::<PathBuf, usize>::new();
    let mut options = entries
        .into_iter()
        .map(|(path, source, exists, parent, is_nested)| {
            let depth = parent
                .as_ref()
                .and_then(|parent| depths.get(parent))
                .copied()
                .unwrap_or(0)
                + usize::from(parent.is_some());
            depths.insert(path.clone(), depth);
            SelectOptionData {
                label: memory_option_label(
                    source,
                    &path,
                    &user_memory_path,
                    &project_memory_path,
                    exists,
                    is_nested,
                    depth,
                ),
                value: path.display().to_string(),
                description: Some(memory_option_description(
                    source,
                    &path,
                    &user_memory_path,
                    &project_memory_path,
                    cwd,
                    parent.as_deref(),
                    is_nested,
                )),
                dim_description: true,
                disabled: false,
                input: None,
            }
        })
        .collect::<Vec<_>>();

    if let Some(path) = auto_memory_path {
        options.push(SelectOptionData {
            label: "Open auto-memory folder".to_string(),
            value: format!("{OPEN_FOLDER_PREFIX}{}", path.display()),
            description: None,
            dim_description: true,
            disabled: false,
            input: None,
        });
    }

    options
}

fn append_agent_memory_folder_options(
    options: &mut Vec<SelectOptionData>,
    cwd: &Path,
    auto_memory_enabled: bool,
) {
    if !auto_memory_enabled {
        return;
    }
    let definitions = get_agent_definitions_with_overrides(cwd);
    for agent in definitions.active_agents {
        let Some(scope) = agent.memory else {
            continue;
        };
        let directory = get_agent_memory_dir(&agent.agent_type, scope, cwd);
        options.push(SelectOptionData {
            label: format!("Open {} agent memory", agent.agent_type),
            value: format!("{OPEN_FOLDER_PREFIX}{}", directory.display()),
            description: Some(format!("{} scope", scope.official_name())),
            dim_description: true,
            ..SelectOptionData::default()
        });
    }
}

fn unix_time_millis(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

fn dream_status(auto_memory_path: Option<&Path>) -> String {
    let Some(path) = auto_memory_path else {
        return String::new();
    };
    match std::fs::metadata(path.join(".consolidate-lock")).and_then(|metadata| metadata.modified())
    {
        Ok(modified) => format!(
            "last ran {}",
            format_relative_time_ago_millis(
                unix_time_millis(modified),
                unix_time_millis(SystemTime::now()),
            )
        ),
        Err(_) => "never".to_string(),
    }
}

fn persist_user_memory_toggle(key: &str, value: bool) {
    let updates = serde_json::Map::from_iter([(key.to_string(), serde_json::Value::Bool(value))]);
    // Official updateSettingsForSource reports through the settings subsystem;
    // the selector keeps its optimistic local state even if persistence fails.
    let _ = update_settings_for_source(SettingSource::User, &updates);
}

fn visible_from_index(focused_index: usize, count: usize, visible_count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    let visible_count = visible_count.max(1).min(count);
    focused_index
        .saturating_add(1)
        .saturating_sub(visible_count)
        .min(count.saturating_sub(visible_count))
}

#[derive(Default, Props)]
pub struct MemoryFileSelectorProps<'a> {
    pub on_select: HandlerMut<'a, String>,
    pub on_cancel: HandlerMut<'a, ()>,
}

/// Maps to: CC `MemoryFileSelector`.
#[component]
pub fn MemoryFileSelector<'a>(
    props: &mut MemoryFileSelectorProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    let cwd = crate::bootstrap::state::get_original_cwd();
    let config_home = crate::utils::env_utils::get_claude_config_home_dir();
    let initial_auto_memory = auto_memory_snapshot();
    let resolved_auto_memory_path = crate::memdir::paths::get_auto_mem_path();
    let mut auto_memory_on = hooks.use_state({
        let enabled = initial_auto_memory.enabled;
        move || enabled
    });
    let mut auto_dream_on = hooks.use_state({
        let enabled = initial_auto_memory.dream_enabled;
        move || enabled
    });
    let show_dream_row = hooks.use_state({
        let enabled = initial_auto_memory.enabled;
        move || enabled
    });
    let existing_files = hooks.use_state(discover_claude_md_files);
    let auto_memory_option_path = auto_memory_on
        .get()
        .then_some(resolved_auto_memory_path.as_path());
    let mut options = memory_select_options(
        &cwd,
        &config_home,
        &existing_files.read(),
        auto_memory_option_path,
    );
    append_agent_memory_folder_options(&mut options, &cwd, auto_memory_on.get());

    let initial_path = LAST_SELECTED_PATH
        .lock()
        .ok()
        .and_then(|path| path.clone())
        .filter(|path| options.iter().any(|option| option.value == *path))
        .or_else(|| options.first().map(|option| option.value.clone()))
        .unwrap_or_default();
    let mut focused_index = hooks.use_state({
        let options = options.clone();
        move || {
            options
                .iter()
                .position(|option| option.value == initial_path)
                .unwrap_or(0)
        }
    });
    let mut focus = hooks.use_state(|| MemoryFocus::Select);
    let mut pending_action = hooks.use_state(|| Option::<MemorySelectorAction>::None);
    let option_count = options.len();
    if focused_index.get() >= option_count {
        focused_index.set(option_count.saturating_sub(1));
    }

    let action = { pending_action.read().clone() };
    if let Some(action) = action {
        pending_action.set(None);
        match action {
            MemorySelectorAction::Cancel => (props.on_cancel)(()),
            MemorySelectorAction::Select(path) => {
                if let Ok(mut last_path) = LAST_SELECTED_PATH.lock() {
                    *last_path = Some(path.clone());
                }
                (props.on_select)(path);
            }
            MemorySelectorAction::OpenFolder(path) => {
                // Official mkdir/openPath chain swallows permission/open errors.
                if std::fs::create_dir_all(&path).is_ok() {
                    let _ = open_path(&path);
                }
            }
            MemorySelectorAction::ToggleAutoMemory => {
                let next = !auto_memory_on.get();
                persist_user_memory_toggle("autoMemoryEnabled", next);
                auto_memory_on.set(next);
            }
            MemorySelectorAction::ToggleAutoDream => {
                let next = !auto_dream_on.get();
                persist_user_memory_toggle("autoDreamEnabled", next);
                auto_dream_on.set(next);
            }
        }
    }

    // Maps to the component-local useExitOnCtrlCDWithKeybindings call.
    let _ = crate::hooks::use_exit::use_exit_on_ctrl_cd_with_keybindings(&mut hooks, true);
    let runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    use_keybinding(
        &mut hooks,
        runtime.clone(),
        "confirm:no",
        ContextName::Confirmation,
        || true,
        {
            let mut pending_action = pending_action;
            move || {
                pending_action.set(Some(MemorySelectorAction::Cancel));
                true
            }
        },
    );
    use_keybinding(
        &mut hooks,
        runtime.clone(),
        "confirm:yes",
        ContextName::Confirmation,
        move || focus.get() != MemoryFocus::Select,
        {
            let mut pending_action = pending_action;
            move || {
                let action = match focus.get() {
                    MemoryFocus::AutoMemory => MemorySelectorAction::ToggleAutoMemory,
                    MemoryFocus::AutoDream => MemorySelectorAction::ToggleAutoDream,
                    MemoryFocus::Select => return false,
                };
                pending_action.set(Some(action));
                true
            }
        },
    );
    use_keybinding(
        &mut hooks,
        runtime.clone(),
        "select:next",
        ContextName::Select,
        || true,
        {
            let mut focus = focus;
            let mut focused_index = focused_index;
            move || {
                match focus.get() {
                    MemoryFocus::AutoMemory if show_dream_row.get() => {
                        focus.set(MemoryFocus::AutoDream)
                    }
                    MemoryFocus::AutoMemory | MemoryFocus::AutoDream => {
                        focus.set(MemoryFocus::Select)
                    }
                    MemoryFocus::Select if option_count > 0 => {
                        focused_index.set((focused_index.get() + 1) % option_count)
                    }
                    MemoryFocus::Select => {}
                }
                true
            }
        },
    );
    use_keybinding(
        &mut hooks,
        runtime.clone(),
        "select:previous",
        ContextName::Select,
        || true,
        {
            let mut focus = focus;
            let mut focused_index = focused_index;
            move || {
                match focus.get() {
                    MemoryFocus::AutoDream => focus.set(MemoryFocus::AutoMemory),
                    MemoryFocus::AutoMemory => {}
                    MemoryFocus::Select if focused_index.get() == 0 => {
                        focus.set(if show_dream_row.get() {
                            MemoryFocus::AutoDream
                        } else {
                            MemoryFocus::AutoMemory
                        })
                    }
                    MemoryFocus::Select => focused_index.set(focused_index.get().saturating_sub(1)),
                }
                true
            }
        },
    );
    let selected_options = options.clone();
    use_keybinding(
        &mut hooks,
        runtime,
        "select:accept",
        ContextName::Select,
        move || focus.get() == MemoryFocus::Select && option_count > 0,
        {
            let mut pending_action = pending_action;
            move || {
                let Some(option) = selected_options.get(focused_index.get()) else {
                    return false;
                };
                let action = option
                    .value
                    .strip_prefix(OPEN_FOLDER_PREFIX)
                    .map(|path| MemorySelectorAction::OpenFolder(PathBuf::from(path)))
                    .unwrap_or_else(|| MemorySelectorAction::Select(option.value.clone()));
                pending_action.set(Some(action));
                true
            }
        },
    );

    let focused = focused_index.get().min(option_count.saturating_sub(1));
    let visible_count = VISIBLE_MEMORY_OPTION_COUNT.min(option_count.max(1));
    let dream_running = hooks
        .try_use_context::<MemoryDreamTaskRunning>()
        .is_some_and(|running| running.0);
    let status = if dream_running {
        "running".to_string()
    } else {
        dream_status(Some(&resolved_auto_memory_path))
    };
    let dream_suffix = if !dream_running && auto_dream_on.get() {
        format!(" · {status} · /dream to run")
    } else {
        format!(" · {status}")
    };

    element! {
        View(flex_direction: FlexDirection::Column, width: 100pct) {
            View(flex_direction: FlexDirection::Column, margin_bottom: 1u32) {
                ListItem(
                    is_focused: focus.get() == MemoryFocus::AutoMemory,
                    label: Some(format!("Auto-memory: {}", if auto_memory_on.get() { "on" } else { "off" })),
                )
                #(if show_dream_row.get() {
                    Some(element! {
                        ListItem(
                            is_focused: focus.get() == MemoryFocus::AutoDream,
                            styled: Some(false),
                        ) {
                            Text(
                                content: format!("Auto-dream: {}", if auto_dream_on.get() { "on" } else { "off" }),
                                color: (focus.get() == MemoryFocus::AutoDream).then_some(theme.suggestion),
                                wrap: TextWrap::NoWrap,
                            )
                            Text(content: dream_suffix, color: theme.inactive, wrap: TextWrap::NoWrap)
                        }
                    })
                } else { None })
            }
            Select(
                options: options,
                focused_index: focused,
                visible_from_index: visible_from_index(focused, option_count, visible_count),
                visible_option_count: visible_count,
                layout: SelectLayout::Compact,
                is_disabled: focus.get() != MemoryFocus::Select,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::settings::SettingsJson;
    use crate::utils::test_env::{EnvVarGuard, HOME_VAR, TEST_ENV_LOCK};
    use crate::utils::theme;
    use futures::{StreamExt, stream};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use uuid::Uuid;

    fn temp_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("cometix-memory-{label}-{}", Uuid::new_v4()))
    }

    fn canvas_lines(canvas: &Canvas) -> Vec<String> {
        (0..canvas.height())
            .map(|y| {
                let mut line = String::new();
                for x in 0..canvas.width() {
                    if let Some(text) = canvas.cell(x, y).and_then(|cell| cell.text()) {
                        line.push_str(text);
                    } else {
                        line.push(' ');
                    }
                }
                line.trim_end().to_string()
            })
            .collect()
    }

    fn render_memory_canvas_after(events: Vec<TerminalEvent>) -> (Canvas, usize) {
        let mut settings = SettingsJson::default();
        settings.auto_memory_enabled = Some(false);
        render_memory_canvas_after_with_settings(events, settings, None)
    }

    /// Renders with `settings` as `getInitialSettings()` and `user` as the
    /// user source's settings (where `getAutoMemPath` finds a trusted
    /// `autoMemoryDirectory`).
    fn render_memory_canvas_after_with_settings(
        events: Vec<TerminalEvent>,
        settings: SettingsJson,
        user: Option<SettingsJson>,
    ) -> (Canvas, usize) {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        crate::memdir::paths::seed_settings(settings, user);
        let close_count = Arc::new(Mutex::new(0usize));
        let close_for_handler = Arc::clone(&close_count);
        let current_theme = *theme::current();

        let canvases = futures::executor::block_on(async move {
            let mut app = element! {
                ContextProvider(value: Context::owned(
                    crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings()
                )) {
                    ContextProvider(value: Context::owned(current_theme)) {
                        MemoryFileSelector(
                            on_cancel: move |_| {
                                *close_for_handler.lock().expect("close mutex") += 1;
                            },
                        )
                    }
                }
            };
            let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                MockTerminalConfig::with_events(stream::iter(events)).with_size(100, 30),
            ));
            let mut canvases = Vec::new();
            loop {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(Duration::from_millis(100)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                canvases.push(canvas);
                if canvases.len() >= 12 {
                    break;
                }
            }
            canvases
        });

        let canvas = canvases
            .last()
            .expect("mock render should produce a canvas")
            .clone();
        let close_count = *close_count.lock().expect("close mutex");
        (canvas, close_count)
    }

    #[test]
    fn memory_selector_options_add_user_and_project_entries_without_writes() {
        let cwd = temp_path("cwd");
        let config_home = temp_path("config");
        let options = memory_select_options(&cwd, &config_home, &[], None);

        assert_eq!(options[0].label, "User memory");
        assert_eq!(
            options[0].description.as_deref(),
            Some("Saved in ~/.claude/CLAUDE.md")
        );
        assert_eq!(options[1].label, "Project memory");
        assert_eq!(
            options[1].description.as_deref(),
            Some("Saved in ./CLAUDE.md")
        );
        assert!(
            !config_home.exists(),
            "option construction must not create config dirs"
        );
        assert!(
            !cwd.exists(),
            "option construction must not create project dirs"
        );
    }

    #[test]
    fn imported_memory_options_render_depth_marker_and_description() {
        let cwd = temp_path("import-cwd");
        let config_home = temp_path("import-config");
        let parent = cwd.join("CLAUDE.md");
        let child = cwd.join("rules.md");
        let files = vec![
            ClaudeMdFile {
                path: parent.clone(),
                source: ClaudeMdSource::Project,
                kind: ClaudeMdKind::Project,
                content: "parent".to_string(),
                parent: None,
                is_nested: false,
            },
            ClaudeMdFile {
                path: child.clone(),
                source: ClaudeMdSource::Project,
                kind: ClaudeMdKind::Project,
                content: "child".to_string(),
                parent: Some(parent),
                is_nested: false,
            },
        ];
        let options = memory_select_options(&cwd, &config_home, &files, None);
        let imported = options
            .iter()
            .find(|option| option.value == child.display().to_string())
            .unwrap();
        assert!(imported.label.starts_with("L "));
        assert_eq!(imported.description.as_deref(), Some("@-imported"));
    }

    #[test]
    fn memory_selector_options_include_auto_memory_folder_when_enabled() {
        let cwd = temp_path("cwd");
        let config_home = temp_path("config");
        let auto_memory_path = temp_path("auto-memory");
        let options = memory_select_options(&cwd, &config_home, &[], Some(&auto_memory_path));

        let folder = options
            .last()
            .expect("auto-memory folder option should be appended");
        assert_eq!(folder.label, "Open auto-memory folder");
        assert_eq!(
            folder.value,
            format!("{OPEN_FOLDER_PREFIX}{}", auto_memory_path.display())
        );
        assert!(
            !auto_memory_path.exists(),
            "folder option construction must stay read-only"
        );
    }

    #[test]
    fn auto_memory_snapshot_matches_official_readonly_env_and_settings_gates() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _env = [
            "CLAUDE_COWORK_MEMORY_PATH_OVERRIDE",
            "CLAUDE_CODE_DISABLE_AUTO_MEMORY",
            "CLAUDE_CODE_SIMPLE",
            "CLAUDE_CODE_REMOTE",
            "CLAUDE_CODE_REMOTE_MEMORY_DIR",
        ]
        .map(EnvVarGuard::unset);
        let _home = EnvVarGuard::set(HOME_VAR, "/tmp");
        let user = Some(SettingsJson {
            auto_memory_directory: Some("~/memory-dir".to_string()),
            ..Default::default()
        });
        let with_auto_memory = |enabled: bool| SettingsJson {
            auto_memory_enabled: Some(enabled),
            auto_dream_enabled: Some(true),
            ..Default::default()
        };

        crate::memdir::paths::seed_settings(with_auto_memory(false), user.clone());
        let disabled = auto_memory_snapshot();
        assert_eq!(disabled.enabled, false);
        assert_eq!(disabled.dream_enabled, false);
        assert_eq!(disabled.path, None);

        crate::memdir::paths::seed_settings(with_auto_memory(true), user.clone());
        let enabled = auto_memory_snapshot();
        assert_eq!(enabled.enabled, true);
        assert_eq!(enabled.dream_enabled, true);
        assert_eq!(enabled.path, Some(PathBuf::from("/tmp/memory-dir")));
        {
            let _env = EnvVarGuard::set("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "true");
            assert!(!auto_memory_snapshot().enabled);
        }

        crate::memdir::paths::seed_settings(with_auto_memory(false), user.clone());
        {
            let _env = EnvVarGuard::set("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "false");
            assert!(auto_memory_snapshot().enabled);
        }

        // With settings on, only the environment gates can turn it off.
        crate::memdir::paths::seed_settings(with_auto_memory(true), user);
        {
            let _env = EnvVarGuard::set("CLAUDE_CODE_SIMPLE", "1");
            assert!(!auto_memory_snapshot().enabled);
        }
        {
            let _env = EnvVarGuard::set("CLAUDE_CODE_REMOTE", "1");
            assert!(!auto_memory_snapshot().enabled);
        }
    }

    #[test]
    fn memory_panel_renders_auto_memory_and_auto_dream_rows_from_readonly_settings() {
        let settings = SettingsJson {
            auto_memory_enabled: Some(true),
            auto_dream_enabled: Some(true),
            ..Default::default()
        };
        let user = SettingsJson {
            auto_memory_directory: Some("/tmp/cometix-auto-memory".to_string()),
            ..Default::default()
        };
        let (canvas, close_count) =
            render_memory_canvas_after_with_settings(Vec::new(), settings, Some(user));
        let text = canvas_lines(&canvas).join("\n");

        assert_eq!(close_count, 0);
        assert!(text.contains("Auto-memory: on"), "canvas=\n{text}");
        assert!(text.contains("Auto-dream: on"), "canvas=\n{text}");
        let options = memory_select_options(
            &crate::bootstrap::state::get_original_cwd(),
            &crate::utils::env_utils::get_claude_config_home_dir(),
            &discover_claude_md_files(),
            Some(Path::new("/tmp/cometix-auto-memory")),
        );
        assert!(
            options
                .iter()
                .any(|option| option.label == "Open auto-memory folder")
        );
        assert!(
            text.contains("never"),
            "missing consolidation lock uses official never status; canvas=\n{text}"
        );
    }

    #[test]
    fn dream_status_matches_official_never_fallback() {
        let path = temp_path("dream-never");
        assert_eq!(dream_status(Some(&path)), "never");
    }

    #[test]
    fn memory_panel_renders_official_dialog_selector_shape() {
        let (canvas, close_count) = render_memory_canvas_after(Vec::new());
        let text = canvas_lines(&canvas).join("\n");

        assert_eq!(close_count, 0);
        assert!(text.contains("Auto-memory: off"), "canvas=\n{text}");
        assert!(text.contains("User memory"), "canvas=\n{text}");
        let options = memory_select_options(
            &crate::bootstrap::state::get_original_cwd(),
            &crate::utils::env_utils::get_claude_config_home_dir(),
            &discover_claude_md_files(),
            None,
        );
        assert!(
            options
                .iter()
                .any(|option| option.label == "Project memory")
        );
        assert!(
            !text.contains("Learn more:"),
            "command-owned documentation belongs outside MemoryFileSelector; canvas=\n{text}"
        );
    }

    #[test]
    fn memory_selector_enter_does_not_cancel() {
        let (_canvas, close_count) = render_memory_canvas_after(vec![TerminalEvent::Key(
            KeyEvent::new(KeyEventKind::Press, KeyCode::Enter),
        )]);
        assert_eq!(close_count, 0);
    }

    #[test]
    fn memory_selector_esc_uses_cancel_callback() {
        let (_canvas, close_count) = render_memory_canvas_after(vec![TerminalEvent::Key(
            KeyEvent::new(KeyEventKind::Press, KeyCode::Esc),
        )]);

        assert_eq!(close_count, 1);
    }
}
