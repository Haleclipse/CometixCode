//! Maps to: CC `tools/ConfigTool/prompt.ts` (the whole file).
//!
//! The `SUPPORTED_SETTINGS` registry this reads lives in
//! [`super::supported_settings`], and `CONFIG_TOOL_NAME` in
//! [`super::constants`] — one Rust file per CC file.

use super::supported_settings::{
    SettingSource, SettingType, get_options_for_setting, supported_settings,
};

/// Maps to: CC `prompt.ts:9` `DESCRIPTION`.
pub const DESCRIPTION: &str = "Get or set Claude Code configuration settings.";

/// Maps to: CC `prompt.ts:14-77` `generatePrompt`.
pub fn generate_prompt() -> String {
    let mut global_settings = Vec::new();
    let mut project_settings = Vec::new();

    for config in supported_settings() {
        // `model` gets its own section with dynamic options.
        if config.key == "model" {
            continue;
        }
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
        "Get or set Claude Code configuration settings.\n\n  View or change Claude Code settings. Use when the user requests configuration changes, asks about current settings, or when adjusting a setting would benefit them.\n\n\n## Usage\n- **Get current value:** Omit the \"value\" parameter\n- **Set new value:** Include the \"value\" parameter\n\n## Configurable settings list\nThe following settings are available for you to change:\n\n### Global Settings (stored in ~/.claude.json)\n{}\n\n### Project Settings (stored in settings.json)\n{}\n\n{}## Examples\n- Get theme: {{ \"setting\": \"theme\" }}\n- Set dark theme: {{ \"setting\": \"theme\", \"value\": \"dark\" }}\n- Enable vim mode: {{ \"setting\": \"editorMode\", \"value\": \"vim\" }}\n- Enable verbose: {{ \"setting\": \"verbose\", \"value\": true }}\n- Change model: {{ \"setting\": \"model\", \"value\": \"opus\" }}\n- Change permission mode: {{ \"setting\": \"permissions.defaultMode\", \"value\": \"plan\" }}\n",
        global_settings.join("\n"),
        project_settings.join("\n"),
        generate_model_section(),
    )
}

/// Maps to: CC `prompt.ts:79-93` `generateModelSection`. The template
/// interpolates `${modelSection}\n## Examples`, so the trailing newline here
/// belongs to the template, not the section.
///
/// Unlike `supported_settings::model_option_values` this keeps the
/// `value: null` default entry and renders it as `null/"default"` (`:83`).
///
/// The `catch` branch (`:89-92`) is reachable only through an unwind: every
/// read in the `get_model_options()` call graph is infallible here, unlike the
/// source's `getSettings_DEPRECATED`/`getModelStrings`/`getGlobalConfig`.
fn generate_model_section() -> String {
    let Ok(options) =
        std::panic::catch_unwind(|| crate::utils::model::model_options::get_model_options(false))
    else {
        return "## Model\n- model - Override the default model (sonnet, opus, haiku, best, or full model ID)\n"
            .to_string();
    };

    let lines = options
        .iter()
        .map(|option| {
            let value = match &option.value {
                Some(value) => format!("\"{value}\""),
                None => "null/\"default\"".to_string(),
            };
            let description = option
                .description_for_model
                .as_deref()
                .unwrap_or(&option.description);
            format!("  - {value}: {description}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("## Model\n- model - Override the default model. Available options:\n{lines}\n")
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::supported_settings;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    /// Pins the process state `getModelOptions()` reads so the section below is
    /// the PAYG first-party list rather than whatever the developer's own
    /// credentials and settings produce.
    pub(in crate::tools::config_tool) struct ModelSectionFixture {
        root: std::path::PathBuf,
        config_dir: Option<EnvVarGuard>,
        cleared_env: Vec<EnvVarGuard>,
    }

    impl ModelSectionFixture {
        pub(in crate::tools::config_tool) fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "cometix-config-prompt-{}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join("settings.json"), "{}").unwrap();
            let config_dir = Some(EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root));

            let cleared_env = [
                "ANTHROPIC_MODEL",
                "ANTHROPIC_DEFAULT_SONNET_MODEL",
                "ANTHROPIC_DEFAULT_OPUS_MODEL",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL",
                "ANTHROPIC_CUSTOM_MODEL_OPTION",
                "CLAUDE_CODE_USE_BEDROCK",
                "CLAUDE_CODE_USE_VERTEX",
                "CLAUDE_CODE_USE_FOUNDRY",
                "CLAUDE_CODE_DISABLE_1M_CONTEXT",
            ]
            .into_iter()
            .map(EnvVarGuard::unset)
            .collect();

            crate::utils::settings::settings_cache::reset_settings_cache();
            crate::utils::config::set_test_global_config(Some(Default::default()));
            crate::bootstrap::state::set_main_loop_model_override(None);
            crate::bootstrap::state::set_initial_main_loop_model(None);

            Self {
                root,
                config_dir,
                cleared_env,
            }
        }

        /// Rewrites the fixture's settings and drops the read-through cache so
        /// the next `getSettings_DEPRECATED()`-equivalent read sees them.
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

    /// Maps to: CC `prompt.ts:79-88` and `supportedSettings.ts:95-99` — both
    /// read `getModelOptions()`, but the prompt keeps the `value: null` default
    /// entry while `getOptions` filters it out.
    ///
    /// The fixture leaves the process without credentials, so the external
    /// build renders the PAYG first-party list; the internal build takes CC's
    /// ant branch (`modelOptions.ts:272-288`) regardless of credentials.
    #[test]
    fn model_prompt_and_options_render_the_official_picker_list() {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let _fixture = ModelSectionFixture::new();

        let (expected_section, expected_values): (&str, &[&str]) = if cfg!(
            feature = "anthropic_internal"
        ) {
            (
                "## Model\n\
                     - model - Override the default model. Available options:\n  \
                     - null/\"default\": Default model (currently Opus 4.6 1M)\n  \
                     - \"opus[1m]\": Opus 4.6 with 1M context - most capable for complex work\n  \
                     - \"sonnet\": Sonnet 4.6 - best for everyday tasks. Generally recommended for most coding tasks\n  \
                     - \"sonnet[1m]\": Sonnet 4.6 with 1M context window - for long sessions with large codebases\n  \
                     - \"haiku\": Haiku 4.5 - fastest for quick answers. Lower cost but less capable than Sonnet 4.6.\n",
                &["opus[1m]", "sonnet", "sonnet[1m]", "haiku"],
            )
        } else {
            (
                "## Model\n\
                     - model - Override the default model. Available options:\n  \
                     - null/\"default\": Use the default model (currently Sonnet 4.6) · $3/$15 per Mtok\n  \
                     - \"sonnet[1m]\": Sonnet 4.6 with 1M context window - for long sessions with large codebases\n  \
                     - \"opus[1m]\": Opus 4.6 with 1M context - most capable for complex work\n  \
                     - \"haiku\": Haiku 4.5 - fastest for quick answers. Lower cost but less capable than Sonnet 4.6.\n",
                &["sonnet[1m]", "opus[1m]", "haiku"],
            )
        };

        assert_eq!(super::generate_model_section(), expected_section);
        assert_eq!(supported_settings::model_option_values(), expected_values);

        // CC interpolates `${modelSection}\n## Examples`; the section carries
        // that separator so the rendered prompt lands byte-for-byte.
        let prompt = super::generate_prompt();
        assert!(prompt.contains(&format!("\n\n{expected_section}## Examples\n")));
        // `model` is excluded from the settings list (CC `prompt.ts:20`).
        assert!(!prompt.contains("- model: "));
    }

    /// Maps to: CC `supportedSettings.ts:100-102` and `prompt.ts:89-92` — the
    /// shared `catch` copy. Both call sites must land on the same fallback.
    #[test]
    fn model_prompt_and_options_keep_the_official_catch_branches() {
        assert_eq!(
            supported_settings::MODEL_OPTIONS_FALLBACK,
            ["sonnet", "opus", "haiku"]
        );
        // Verbatim from `prompt.ts:90-91`, plus the template's separator.
        assert!(
            super::generate_model_section()
                .starts_with("## Model\n- model - Override the default model")
        );
    }
}
