//! Output-style directory loader.
//! Maps to: CC `outputStyles/loadOutputStylesDir.ts`.

use crate::constants::output_styles::OutputStyleConfig;
use crate::utils::frontmatter_parser::coerce_description_to_string;
use crate::utils::markdown_config_loader::extract_description_from_markdown;
use crate::utils::markdown_config_loader::load_markdown_files_for_subdir;
use serde_json::Value;
use std::path::Path;

fn frontmatter_string<'a>(
    frontmatter: &'a crate::utils::frontmatter_parser::FrontmatterData,
    key: &str,
) -> Option<&'a str> {
    frontmatter
        .get(key)?
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn keep_coding_instructions(value: Option<&Value>) -> Option<bool> {
    match value? {
        Value::Bool(value) => Some(*value),
        Value::String(value) if value == "true" => Some(true),
        Value::String(value) if value == "false" => Some(false),
        _ => None,
    }
}

/// Maps to: CC `outputStyles/loadOutputStylesDir.ts#getOutputStyleDirStyles`.
pub fn get_output_style_dir_styles(cwd: &Path) -> Vec<OutputStyleConfig> {
    load_markdown_files_for_subdir("output-styles", cwd)
        .into_iter()
        .filter_map(|file| {
            let file_name = file.file_path.file_name()?.to_string_lossy();
            let style_name = file_name.strip_suffix(".md").unwrap_or(&file_name);
            let name = frontmatter_string(&file.frontmatter, "name")
                .unwrap_or(style_name)
                .to_string();
            let description = coerce_description_to_string(file.frontmatter.get("description"))
                .unwrap_or_else(|| {
                    extract_description_from_markdown(
                        &file.content,
                        &format!("Custom {style_name} output style"),
                    )
                });
            Some(OutputStyleConfig {
                name,
                description,
                prompt: file.content.trim().to_string(),
                source: file.source.as_str().to_string(),
                keep_coding_instructions: keep_coding_instructions(
                    file.frontmatter.get("keep-coding-instructions"),
                ),
                force_for_plugin: None,
            })
        })
        .collect()
}

/// Maps to: CC `outputStyles/loadOutputStylesDir.ts#clearOutputStyleCaches`.
/// Current Rust loader is synchronous and uncached, so clearing is a safe no-op.
pub fn clear_output_style_caches() {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn output_style_dir_styles_map_frontmatter_like_official_loader() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("cometix-output-style-dir-{}", uuid::Uuid::new_v4()));
        let cwd = root.join("repo");
        let config_home = root.join("config");
        std::fs::create_dir_all(cwd.join(".git")).unwrap();
        std::fs::create_dir_all(cwd.join(".claude/output-styles")).unwrap();
        std::fs::create_dir_all(config_home.join("output-styles")).unwrap();
        std::fs::write(
            cwd.join(".claude/output-styles/mentor.md"),
            "---\nname: Mentor\ndescription: 123\nkeep-coding-instructions: 'false'\nforce-for-plugin: true\n---\n# Mentor prompt\nBody",
        )
        .unwrap();
        let _config_guard = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &config_home);
        let old_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(&cwd).unwrap();
        let styles = get_output_style_dir_styles(&cwd);
        std::env::set_current_dir(old_cwd).unwrap();
        let _ = std::fs::remove_dir_all(root);

        let style = styles.iter().find(|style| style.name == "Mentor").unwrap();
        assert_eq!(style.description, "123");
        assert_eq!(style.prompt, "# Mentor prompt\nBody");
        assert_eq!(style.source, "projectSettings");
        assert_eq!(style.keep_coding_instructions, Some(false));
        assert_eq!(style.force_for_plugin, None);
    }
}
