//! Maps to: CC `tools/ConfigTool/prompt.ts` (the whole file).
//!
//! The `SUPPORTED_SETTINGS` registry this reads lives in
//! [`super::supported_settings`], and `CONFIG_TOOL_NAME` in [`super::constants`]
//! — one Rust file per CC file.

use super::supported_settings::{
    SettingSource, SettingType, get_options_for_setting, supported_settings,
};

/// Maps to: CC `prompt.ts:9` `DESCRIPTION`.
pub const DESCRIPTION: &str = "Get or set nyan configuration settings.";

/// Maps to: CC `prompt.ts:14-77` `generatePrompt`.
pub fn generate_prompt() -> String {
    let mut global_settings = Vec::new();
    let mut project_settings = Vec::new();

    for config in supported_settings() {
        // Voice settings are registered at build time but gated by GrowthBook at
        // runtime; hide them from the prompt when the kill-switch is on.
        if cfg!(feature = "voice_mode")
            && config.key == "voiceEnabled"
            && !crate::voice::voice_mode_enabled::is_voice_growth_book_enabled()
        {
            continue;
        }

        let mut line = format!("- {}", config.key);
        if let Some(options) = get_options_for_setting(config.key) {
            line.push_str(&format!(
                ": {}",
                options
                    .iter()
                    .map(|option| format!("\"{option}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        } else if config.setting_type == SettingType::Boolean {
            line.push_str(": true/false");
        }
        line.push_str(&format!(" - {}", config.description));

        if config.source == SettingSource::Global {
            global_settings.push(line);
        } else {
            project_settings.push(line);
        }
    }

    format!(
        "Get or set nyan configuration settings.\n\n  View or change nyan settings. Use when the user requests configuration changes, asks about current settings, or when adjusting a setting would benefit them.\n\n\n## Usage\n- **Get current value:** Omit the \"value\" parameter\n- **Set new value:** Include the \"value\" parameter\n\n## Configurable settings list\nThe following settings are available for you to change:\n\n### Global Settings (stored in ~/.claude.json)\n{}\n\n### Project Settings (stored in settings.json)\n{}\n\n## Examples\n- Get theme: {{ \"setting\": \"theme\" }}\n- Set dark theme: {{ \"setting\": \"theme\", \"value\": \"dark\" }}\n- Enable vim mode: {{ \"setting\": \"editorMode\", \"value\": \"vim\" }}\n- Enable verbose: {{ \"setting\": \"verbose\", \"value\": true }}\n- Enable plan mode permissions: {{ \"setting\": \"permissions.defaultMode\", \"value\": \"plan\" }}\n",
        global_settings.join("\n"),
        project_settings.join("\n"),
    )
}

#[cfg(test)]
pub(super) mod tests {
    use crate::utils::test_env::EnvVarGuard;

    pub(in crate::tools::config_tool) struct ModelSectionFixture {
        root: std::path::PathBuf,
        config_dir: Option<EnvVarGuard>,
        cleared_env: Vec<EnvVarGuard>,
    }

    impl ModelSectionFixture {
        pub(in crate::tools::config_tool) fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "nyan-config-prompt-{}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join("settings.json"), "{}").unwrap();
            let config_dir = Some(EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root));
            let cleared_env = [
                "CLAUDE_CODE_USE_BEDROCK",
                "CLAUDE_CODE_USE_VERTEX",
                "CLAUDE_CODE_USE_FOUNDRY",
            ]
            .into_iter()
            .map(EnvVarGuard::unset)
            .collect();

            crate::utils::settings::settings_cache::reset_settings_cache();
            crate::utils::config::set_test_global_config(Some(Default::default()));
            crate::bootstrap::state::set_session_profile(None);

            Self {
                root,
                config_dir,
                cleared_env,
            }
        }

        pub(in crate::tools::config_tool) fn write_settings(&self, settings: &str) {
            std::fs::write(self.root.join("settings.json"), settings).unwrap();
            crate::utils::settings::settings_cache::reset_settings_cache();
        }
    }

    impl Drop for ModelSectionFixture {
        fn drop(&mut self) {
            self.cleared_env.clear();
            drop(self.config_dir.take());
            crate::utils::settings::settings_cache::reset_settings_cache();
            crate::utils::config::set_test_global_config(None);
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}
