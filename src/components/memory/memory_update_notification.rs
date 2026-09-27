//! Maps to: CC `components/memory/MemoryUpdateNotification.tsx:1-42`.

use crate::utils::theme::Theme;
use iocraft::prelude::*;
use std::path::{Component, Path, PathBuf};

fn lexical_components(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().to_string()),
            _ => None,
        })
        .collect()
}

fn relative_path(from: &Path, to: &Path) -> String {
    let from_parts = lexical_components(from);
    let to_parts = lexical_components(to);
    let common = from_parts
        .iter()
        .zip(&to_parts)
        .take_while(|(left, right)| left == right)
        .count();
    let mut parts = vec!["..".to_string(); from_parts.len().saturating_sub(common)];
    parts.extend(to_parts.into_iter().skip(common));
    if parts.is_empty() {
        String::new()
    } else {
        parts.join(std::path::MAIN_SEPARATOR_STR)
    }
}

fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

/// Maps to: CC `getRelativeMemoryPath(path)`.
pub fn get_relative_memory_path_with_roots(path: &Path, home: &Path, cwd: &Path) -> String {
    let path_text = path.to_string_lossy();
    let home_text = home.to_string_lossy();
    let cwd_text = cwd.to_string_lossy();

    // Preserve the official string-prefix gates before path.relative().
    let relative_to_home = path_text
        .starts_with(home_text.as_ref())
        .then(|| format!("~{}", &path_text[home_text.len()..]));
    let relative_to_cwd = path_text
        .starts_with(cwd_text.as_ref())
        .then(|| format!("./{}", relative_path(cwd, path)));

    match (relative_to_home, relative_to_cwd) {
        (Some(home), Some(cwd)) => {
            if utf16_len(&home) <= utf16_len(&cwd) {
                home
            } else {
                cwd
            }
        }
        (Some(home), None) => home,
        (None, Some(cwd)) => cwd,
        (None, None) => path_text.into_owned(),
    }
}

/// Maps to: CC `getRelativeMemoryPath(path)` using process roots.
pub fn get_relative_memory_path(path: &Path) -> String {
    let home = crate::utils::process_env::var_os("HOME")
        .or_else(|| crate::utils::process_env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default();
    let cwd = std::env::current_dir().unwrap_or_default();
    get_relative_memory_path_with_roots(path, &home, &cwd)
}

#[derive(Default, Props)]
pub struct MemoryUpdateNotificationProps {
    pub memory_path: String,
}

/// Maps to: CC `MemoryUpdateNotification`.
#[component]
pub fn MemoryUpdateNotification(
    props: &MemoryUpdateNotificationProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let display_path = get_relative_memory_path(Path::new(&props.memory_path));
    element! {
        View(flex_direction: FlexDirection::Column, flex_grow: 1.0f32) {
            Text(
                content: format!("Memory updated in {display_path} · /memory to edit"),
                color: theme.text,
                wrap: TextWrap::Wrap,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    #[test]
    fn relative_memory_path_matches_official_shortest_prefix_choice() {
        let home = Path::new("/home/alice");
        let cwd = Path::new("/home/alice/work/repo");
        assert_eq!(
            get_relative_memory_path_with_roots(
                Path::new("/home/alice/work/repo/CLAUDE.md"),
                home,
                cwd,
            ),
            "./CLAUDE.md"
        );
        assert_eq!(
            get_relative_memory_path_with_roots(
                Path::new("/home/alice/.claude/CLAUDE.md"),
                home,
                cwd,
            ),
            "~/.claude/CLAUDE.md"
        );
        assert_eq!(
            get_relative_memory_path_with_roots(Path::new("/etc/claude/CLAUDE.md"), home, cwd),
            "/etc/claude/CLAUDE.md"
        );
    }

    #[test]
    fn memory_update_notification_matches_official_copy() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                MemoryUpdateNotification(memory_path: "/etc/claude/CLAUDE.md".to_string())
            }
        }
        .render(Some(90))
        .to_string();
        assert!(
            text.contains("Memory updated in /etc/claude/CLAUDE.md · /memory to edit"),
            "canvas=\n{text}"
        );
    }
}
