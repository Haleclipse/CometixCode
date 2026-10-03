//! Maps to: CC `commands/vim/index.ts:3-9` + `commands/vim/vim.ts:8-38`.
//!
//! The config mutation stays with the command owner. PromptInput already reads
//! the cached global `editorMode`, so `save_global_config` makes the toggle live
//! without introducing a second editor-mode state source. Analytics is omitted
//! with the project-wide telemetry scope.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VimModeChange {
    pub mode: String,
    pub output: String,
}

/// Maps to: CC `commands/vim/vim.ts:10-17`, including legacy `emacs` handling.
pub fn next_editor_mode(current_mode: Option<&str>) -> VimModeChange {
    let current_mode = match current_mode.unwrap_or("normal") {
        "emacs" => "normal",
        mode => mode,
    };
    let mode = if current_mode == "normal" {
        "vim"
    } else {
        "normal"
    };
    let guidance = if mode == "vim" {
        "Use Escape key to toggle between INSERT and NORMAL modes."
    } else {
        "Using standard (readline) keyboard bindings."
    };
    VimModeChange {
        mode: mode.to_string(),
        output: format!("Editor mode set to {mode}. {guidance}"),
    }
}

/// Maps to: CC `commands/vim/vim.ts:8-38` `call`.
pub fn call() -> anyhow::Result<VimModeChange> {
    let config = crate::utils::config::load_global_config();
    let change = next_editor_mode(config.editor_mode.as_deref());
    let mode = change.mode.clone();
    crate::utils::config::save_global_config(move |config| {
        config.editor_mode = Some(mode);
    })?;
    Ok(change)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK};

    #[test]
    fn editor_mode_toggle_matches_official_normal_vim_cycle() {
        assert_eq!(
            next_editor_mode(None),
            VimModeChange {
                mode: "vim".to_string(),
                output: "Editor mode set to vim. Use Escape key to toggle between INSERT and NORMAL modes."
                    .to_string(),
            }
        );
        assert_eq!(
            next_editor_mode(Some("vim")),
            VimModeChange {
                mode: "normal".to_string(),
                output: "Editor mode set to normal. Using standard (readline) keyboard bindings."
                    .to_string(),
            }
        );
    }

    #[test]
    fn vim_call_persists_official_editor_mode_cycle() {
        struct RestoreEnv {
            config_dir: Option<EnvVarGuard>,
            write_enabled: Option<EnvVarGuard>,
            root: std::path::PathBuf,
        }
        impl Drop for RestoreEnv {
            fn drop(&mut self) {
                drop(self.config_dir.take());
                drop(self.write_enabled.take());
                crate::utils::config::clear_global_config_cache_for_testing();
                let _ = std::fs::remove_dir_all(&self.root);
            }
        }

        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let root =
            std::env::temp_dir().join(format!("cometix-vim-command-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(".claude.json"), r#"{"editorMode":"normal"}"#).unwrap();
        let _restore = RestoreEnv {
            config_dir: Some(EnvVarGuard::set("CLAUDE_CONFIG_DIR", &root)),
            write_enabled: Some(EnvVarGuard::set("COMETIX_WRITE_ENABLED", "1")),
            root: root.clone(),
        };
        crate::utils::config::clear_global_config_cache_for_testing();

        assert_eq!(call().unwrap().mode, "vim");
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(root.join(".claude.json")).unwrap())
                .unwrap();
        assert_eq!(written["editorMode"], "vim");
        assert_eq!(call().unwrap().mode, "normal");
    }

    #[test]
    fn legacy_emacs_mode_is_treated_as_normal_like_official() {
        assert_eq!(next_editor_mode(Some("emacs")).mode, "vim");
        assert_eq!(next_editor_mode(Some("unknown")).mode, "normal");
    }
}
