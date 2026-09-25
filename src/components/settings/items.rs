use crate::utils::config::GlobalConfig;
use crate::utils::env_utils;
use crate::utils::settings::types::SettingsJson;
use crate::utils::theme;

#[derive(Debug, Clone)]
pub enum SettingValue {
    Bool(bool),
    Enum {
        options: Vec<String>,
        current: usize,
    },
    Display(String),
}

#[derive(Debug, Clone)]
pub struct SettingItem {
    pub id: &'static str,
    pub label: &'static str,
    pub value: SettingValue,
    pub search_text: &'static str,
    pub visible: bool,
}

impl SettingItem {
    pub fn display_value(&self) -> String {
        match &self.value {
            SettingValue::Bool(v) => v.to_string(),
            SettingValue::Enum { options, current } => {
                options.get(*current).cloned().unwrap_or_default()
            }
            SettingValue::Display(s) => s.clone(),
        }
    }

    pub fn is_managed(&self) -> bool {
        matches!(self.value, SettingValue::Display(_))
    }

    pub fn toggle(&mut self) {
        match &mut self.value {
            SettingValue::Bool(v) => *v = !*v,
            SettingValue::Enum { options, current } => {
                if !options.is_empty() {
                    *current = (*current + 1) % options.len();
                }
            }
            SettingValue::Display(_) => {}
        }
    }

    pub fn next_value(&mut self) {
        match &mut self.value {
            SettingValue::Bool(v) => *v = true,
            SettingValue::Enum { options, current } => {
                if !options.is_empty() && *current + 1 < options.len() {
                    *current += 1;
                }
            }
            SettingValue::Display(_) => {}
        }
    }

    pub fn prev_value(&mut self) {
        match &mut self.value {
            SettingValue::Bool(v) => *v = false,
            SettingValue::Enum { options, current } => {
                if !options.is_empty() && *current > 0 {
                    *current -= 1;
                }
            }
            SettingValue::Display(_) => {}
        }
    }
}

fn auto_updates_are_disabled(config: &GlobalConfig) -> bool {
    config.auto_updates == Some(false)
        || crate::utils::env_utils::is_env_truthy(
            std::env::var("DISABLE_AUTOUPDATER").ok().as_deref(),
        )
        || crate::utils::env_utils::is_env_truthy(
            std::env::var("CLAUDE_CODE_DISABLE_AUTOUPDATER")
                .ok()
                .as_deref(),
        )
        || crate::utils::env_utils::is_env_truthy(
            std::env::var("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC")
                .ok()
                .as_deref(),
        )
}

fn auto_updates_channel_display(config: &GlobalConfig, settings: &SettingsJson) -> String {
    if auto_updates_are_disabled(config) {
        "disabled".to_string()
    } else {
        settings
            .auto_updates_channel
            .as_deref()
            .unwrap_or("latest")
            .to_string()
    }
}

pub fn notification_channel_display(value: &str) -> String {
    match value.trim() {
        "auto" | "Auto" | "" => "Auto".to_string(),
        "iterm2" | "iTerm2" | "ITerm2" => "iTerm2 (OSC 9)".to_string(),
        "terminal_bell" | "Terminal bell" | "Terminal Bell" | "Bell" => {
            "Terminal Bell (\\a)".to_string()
        }
        "iterm2_with_bell" | "iTerm2 with bell" | "ITerm2 with bell" => {
            "iTerm2 w/ Bell".to_string()
        }
        "kitty" | "Kitty" => "Kitty (OSC 99)".to_string(),
        "ghostty" | "Ghostty" => "Ghostty (OSC 777)".to_string(),
        "notifications_disabled" | "Disabled" | "disabled" => "Disabled".to_string(),
        other => other.to_string(),
    }
}

pub fn build_items(config: &GlobalConfig, settings: &SettingsJson) -> Vec<SettingItem> {
    vec![
        // ════════════════════════════════════════════════════
        // ════════════════════════════════════════════════════
        SettingItem {
            id: "autoCompactEnabled",
            label: "Auto-compact",
            value: SettingValue::Bool(config.auto_compact_enabled.unwrap_or(true)),
            search_text: "compact context window auto",
            visible: true,
        },
        SettingItem {
            id: "spinnerTipsEnabled",
            label: "Show tips",
            value: SettingValue::Bool(settings.spinner_tips_enabled.unwrap_or(true)),
            search_text: "tips hints help spinner",
            visible: true,
        },
        SettingItem {
            id: "prefersReducedMotion",
            label: "Reduce motion",
            value: SettingValue::Bool(settings.prefers_reduced_motion.unwrap_or(false)),
            search_text: "animation motion reduce accessibility",
            visible: true,
        },
        // @cometix offset: not an official setting. CC previews only completed
        // lines while streaming; Cometix also offers character streaming.
        // Reduce motion hides the preview regardless (CC REPL.tsx:1987).
        SettingItem {
            id: "streamingTextDisplay",
            label: "Streaming text",
            value: {
                let options = vec!["character".to_string(), "line".to_string()];
                let current = settings
                    .streaming_text_display
                    .as_deref()
                    .and_then(|mode| options.iter().position(|o| o == mode))
                    .unwrap_or(0);
                SettingValue::Enum { options, current }
            },
            search_text: "streaming text character line typewriter preview",
            visible: true,
        },
        SettingItem {
            id: "thinkingEnabled",
            label: "Thinking mode",
            value: SettingValue::Bool(settings.always_thinking_enabled.unwrap_or(true)),
            search_text: "thinking extended",
            visible: true,
        },
        // condition: isFastModeEnabled() && isFastModeAvailable()
        SettingItem {
            id: "fastMode",
            label: "Fast mode (Opus 4.6 only)",
            value: SettingValue::Bool(settings.fast_mode.unwrap_or(false)),
            search_text: "fast mode speed opus",
            visible: true,
        },
        // condition: tengu_chomp_inflection feature
        SettingItem {
            id: "promptSuggestionEnabled",
            label: "Prompt suggestions",
            value: SettingValue::Bool(settings.prompt_suggestion_enabled.unwrap_or(false)),
            search_text: "prompt suggestion inflection",
            visible: false,
        },
        // condition: ant-only (never in external builds)
        SettingItem {
            id: "speculationEnabled",
            label: "Speculative execution",
            value: SettingValue::Bool(false),
            search_text: "speculation speculative execution",
            visible: false, // ant-only
        },
        SettingItem {
            id: "fileCheckpointingEnabled",
            label: "Rewind code (checkpoints)",
            value: SettingValue::Bool(config.file_checkpointing_enabled.unwrap_or(true)),
            search_text: "rewind checkpoint undo file",
            visible: true,
        },
        SettingItem {
            id: "verbose",
            label: "Verbose output",
            value: SettingValue::Bool(config.verbose.unwrap_or(false)),
            search_text: "verbose debug output log",
            visible: true,
        },
        SettingItem {
            id: "expandThinking",
            label: "Expand thinking blocks",
            value: SettingValue::Bool(config.expand_thinking.unwrap_or(true)),
            search_text: "thinking expand collapse ctrl+o reasoning",
            visible: true,
        },
        SettingItem {
            id: "expandCollapsedReadSearch",
            label: "Expand read/search groups",
            value: SettingValue::Bool(config.expand_collapsed_read_search.unwrap_or(true)),
            search_text: "read search collapsed expand tool group ctrl+o",
            visible: true,
        },
        SettingItem {
            id: "terminalProgressBarEnabled",
            label: "Terminal progress bar",
            value: SettingValue::Bool(config.terminal_progress_bar_enabled.unwrap_or(false)),
            search_text: "terminal progress bar osc",
            visible: true,
        },
        // condition: tengu_terminal_sidebar feature
        SettingItem {
            id: "showStatusInTerminalTab",
            label: "Show status in terminal tab",
            value: SettingValue::Bool(false),
            search_text: "status terminal tab sidebar",
            visible: false, // feature-gated
        },
        SettingItem {
            id: "showTurnDuration",
            label: "Show turn duration",
            value: SettingValue::Bool(config.show_turn_duration.unwrap_or(false)),
            search_text: "turn duration time",
            visible: true,
        },
        SettingItem {
            id: "defaultPermissionMode",
            label: "Default permission mode",
            value: {
                let mode = settings
                    .permissions
                    .as_ref()
                    .and_then(|p| p.default_mode.as_deref())
                    .or(settings.default_permission_mode.as_deref())
                    .unwrap_or("default");
                let options = vec![
                    "default".to_string(),
                    "plan".to_string(),
                    "acceptEdits".to_string(),
                    "dontAsk".to_string(),
                ];
                let current = options.iter().position(|o| o == mode).unwrap_or(0);
                SettingValue::Enum { options, current }
            },
            search_text: "permission mode auto plan accept default",
            visible: true,
        },
        // condition: TRANSCRIPT_CLASSIFIER feature
        SettingItem {
            id: "useAutoModeDuringPlan",
            label: "Use auto mode during plan",
            value: SettingValue::Bool(settings.use_auto_mode_during_plan.unwrap_or(true)),
            search_text: "auto mode plan",
            visible: false, // feature-gated (TRANSCRIPT_CLASSIFIER)
        },
        SettingItem {
            id: "respectGitignore",
            label: "Respect .gitignore in file picker",
            value: SettingValue::Bool(settings.respect_gitignore.unwrap_or(true)),
            search_text: "gitignore ignore file filter picker",
            visible: true,
        },
        SettingItem {
            id: "copyFullResponse",
            label: "Always copy full response",
            value: SettingValue::Bool(config.copy_full_response.unwrap_or(false)),
            search_text: "copy full response clipboard",
            visible: true,
        },
        // Hidden until in-app selection support is implemented.
        SettingItem {
            id: "copyOnSelect",
            label: "Copy on select",
            value: SettingValue::Bool(true),
            search_text: "copy select clipboard",
            visible: false,
        },
        SettingItem {
            id: "autoUpdatesChannel",
            label: "Auto-update channel",
            value: SettingValue::Display(auto_updates_channel_display(config, settings)),
            search_text: "auto update channel version",
            visible: true,
        },
        // ════════════════════════════════════════════════════
        // ════════════════════════════════════════════════════
        SettingItem {
            id: "theme",
            label: "Theme",
            value: SettingValue::Display(
                theme::theme_display_label(config.theme.as_deref()).to_string(),
            ),
            search_text: "theme color appearance dark light ansi daltonized",
            visible: true,
        },
        SettingItem {
            id: "notifChannel",
            label: "Notifications",
            value: {
                let channel = config.preferred_notif_channel.as_deref().unwrap_or("auto");
                let options = vec![
                    "auto".to_string(),
                    "iterm2".to_string(),
                    "terminal_bell".to_string(),
                    "iterm2_with_bell".to_string(),
                    "kitty".to_string(),
                    "ghostty".to_string(),
                    "notifications_disabled".to_string(),
                ];
                let current = options
                    .iter()
                    .position(|option| option == channel)
                    .unwrap_or(0);
                SettingValue::Enum { options, current }
            },
            search_text: "notification bell sound alert iterm kitty ghostty",
            visible: true,
        },
        // condition: KAIROS || KAIROS_PUSH_NOTIFICATION
        SettingItem {
            id: "taskCompleteNotifEnabled",
            label: "Push when idle",
            value: SettingValue::Bool(false),
            search_text: "push notification idle complete",
            visible: false, // feature-gated
        },
        // condition: KAIROS || KAIROS_PUSH_NOTIFICATION
        SettingItem {
            id: "inputNeededNotifEnabled",
            label: "Push when input needed",
            value: SettingValue::Bool(false),
            search_text: "push notification input needed",
            visible: false, // feature-gated
        },
        // condition: KAIROS || KAIROS_PUSH_NOTIFICATION
        SettingItem {
            id: "agentPushNotifEnabled",
            label: "Push when Claude decides",
            value: SettingValue::Bool(false),
            search_text: "push notification agent decides",
            visible: false, // feature-gated
        },
        SettingItem {
            id: "outputStyle",
            label: "Output style",
            value: SettingValue::Display({
                let style = settings.output_style.as_deref().unwrap_or("default");
                if style.eq_ignore_ascii_case("default") {
                    "Default".to_string()
                } else {
                    style.to_string()
                }
            }),
            search_text: "output style format",
            visible: true,
        },
        // condition: KAIROS || KAIROS_BRIEF
        SettingItem {
            id: "defaultView",
            label: "What you see by default",
            value: {
                let view = settings.default_view.as_deref().unwrap_or("transcript");
                let options = vec!["transcript".to_string(), "chat".to_string()];
                let current = options.iter().position(|o| o == view).unwrap_or(0);
                SettingValue::Enum { options, current }
            },
            search_text: "default view transcript chat",
            visible: false, // feature-gated (KAIROS)
        },
        SettingItem {
            id: "language",
            label: "Language",
            value: SettingValue::Display(
                settings
                    .language
                    .as_deref()
                    .unwrap_or("Default (English)")
                    .to_string(),
            ),
            search_text: "language locale chinese japanese",
            visible: true,
        },
        SettingItem {
            id: "editorMode",
            label: "Editor mode",
            value: {
                let mode = config.editor_mode.as_deref().unwrap_or("normal");
                let effective = if mode == "emacs" { "normal" } else { mode };
                let options = vec!["normal".to_string(), "vim".to_string()];
                let current = options.iter().position(|o| o == effective).unwrap_or(0);
                SettingValue::Enum { options, current }
            },
            search_text: "editor vim keybinding emacs",
            visible: true,
        },
        SettingItem {
            id: "prStatusFooterEnabled",
            label: "Show PR status footer",
            value: SettingValue::Bool(config.pr_status_footer_enabled.unwrap_or(true)),
            search_text: "pr pull request status footer badge",
            visible: true,
        },
        SettingItem {
            id: "model",
            label: "Model",
            value: SettingValue::Display(
                settings
                    .model
                    .as_deref()
                    .unwrap_or("Default (recommended)")
                    .to_string(),
            ),
            search_text: "model ai claude sonnet opus haiku",
            visible: true,
        },
        // condition: hasAccessToIDEExtensionDiffFeature()
        SettingItem {
            id: "diffTool",
            label: "Diff tool",
            value: {
                let options = vec!["auto".to_string(), "terminal".to_string()];
                SettingValue::Enum {
                    options,
                    current: 0,
                }
            },
            search_text: "diff tool ide terminal",
            visible: false,
        },
        // condition: !isSupportedTerminal()
        SettingItem {
            id: "autoConnectIde",
            label: "Auto-connect to IDE (external terminal)",
            value: SettingValue::Bool(config.auto_connect_ide.unwrap_or(false)),
            search_text: "auto connect ide extension external terminal",
            visible: false,
        },
        // condition: isSupportedTerminal()
        SettingItem {
            id: "autoInstallIdeExtension",
            label: "Auto-install IDE extension",
            value: SettingValue::Bool(config.auto_install_ide_extension.unwrap_or(true)),
            search_text: "auto install ide extension",
            visible: false,
        },
        SettingItem {
            id: "claudeInChromeDefaultEnabled",
            label: "Claude in Chrome enabled by default",
            value: SettingValue::Bool(true),
            search_text: "chrome browser extension default",
            visible: true,
        },
        // condition: isAgentSwarmsEnabled()
        SettingItem {
            id: "teammateMode",
            label: "Teammate mode",
            value: {
                let options = vec![
                    "auto".to_string(),
                    "tmux".to_string(),
                    "in-process".to_string(),
                ];
                SettingValue::Enum {
                    options,
                    current: 0,
                }
            },
            search_text: "teammate mode swarm agent tmux",
            visible: false, // condition: isAgentSwarmsEnabled()
        },
        // condition: isAgentSwarmsEnabled()
        SettingItem {
            id: "teammateDefaultModel",
            label: "Default teammate model",
            value: SettingValue::Display("Default".to_string()),
            search_text: "teammate default model",
            visible: false, // condition: isAgentSwarmsEnabled()
        },
        // condition: BRIDGE_MODE && isBridgeEnabled()
        SettingItem {
            id: "remoteControlAtStartup",
            label: "Enable Remote Control for all sessions",
            value: {
                let options = vec![
                    "default".to_string(),
                    "true".to_string(),
                    "false".to_string(),
                ];
                SettingValue::Enum {
                    options,
                    current: 0,
                }
            },
            search_text: "remote control bridge session",
            visible: false, // feature-gated (BRIDGE_MODE)
        },
        // condition: hasExternalClaudeMdIncludes()
        SettingItem {
            id: "showExternalIncludesDialog",
            label: "External CLAUDE.md includes",
            value: SettingValue::Display("true".to_string()),
            search_text: "external claude md includes",
            visible: false,
        },
        // condition: ANTHROPIC_API_KEY env var
        SettingItem {
            id: "apiKey",
            label: "Use custom API key",
            value: SettingValue::Bool(false),
            search_text: "custom api key anthropic",
            visible: false,
        },
    ]
}

pub fn default_items() -> Vec<SettingItem> {
    build_items(&GlobalConfig::default(), &SettingsJson::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_channel_display_matches_official_config_values() {
        assert_eq!(notification_channel_display("auto"), "Auto");
        assert_eq!(notification_channel_display("iterm2"), "iTerm2 (OSC 9)");
        assert_eq!(
            notification_channel_display("terminal_bell"),
            "Terminal Bell (\\a)"
        );
        assert_eq!(notification_channel_display("kitty"), "Kitty (OSC 99)");
        assert_eq!(
            notification_channel_display("notifications_disabled"),
            "Disabled"
        );

        let mut config = GlobalConfig::default();
        config.preferred_notif_channel = Some("kitty".to_string());
        let row = build_items(&config, &SettingsJson::default())
            .into_iter()
            .find(|item| item.id == "notifChannel")
            .expect("notification channel row should exist");
        assert_eq!(row.display_value(), "kitty");
        assert!(matches!(row.value, SettingValue::Enum { .. }));
    }

    #[test]
    fn default_permission_mode_options_follow_official_external_order() {
        let row = default_items()
            .into_iter()
            .find(|item| item.id == "defaultPermissionMode")
            .expect("default permission mode row should exist");
        let SettingValue::Enum { options, current } = row.value else {
            panic!("default permission mode should be an enum row");
        };
        assert_eq!(options, vec!["default", "plan", "acceptEdits", "dontAsk"]);
        assert_eq!(current, 0);
    }

    struct EnvUnsetGuard {
        _env: crate::utils::env_utils::EnvVarGuard,
    }

    impl EnvUnsetGuard {
        fn unset(key: &'static str) -> Self {
            Self {
                _env: crate::utils::env_utils::EnvVarGuard::unset(key),
            }
        }
    }

    #[test]
    fn auto_updates_channel_display_tracks_official_disabled_value() {
        // `auto_updates_are_disabled` consults machine-level env
        // kill-switches; neutralize them so the config/settings inputs
        // under test decide the outcome on every host.
        let _env_lock = crate::utils::env_utils::TEST_ENV_LOCK.lock().unwrap();
        let _disable_guard = EnvUnsetGuard::unset("DISABLE_AUTOUPDATER");
        let _claude_disable_guard = EnvUnsetGuard::unset("CLAUDE_CODE_DISABLE_AUTOUPDATER");
        let _traffic_guard = EnvUnsetGuard::unset("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC");
        let mut config = GlobalConfig::default();
        let mut settings = SettingsJson::default();
        settings.auto_updates_channel = Some("stable".to_string());

        assert_eq!(auto_updates_channel_display(&config, &settings), "stable");

        config.auto_updates = Some(false);
        assert_eq!(auto_updates_channel_display(&config, &settings), "disabled");

        let row = build_items(&config, &settings)
            .into_iter()
            .find(|item| item.id == "autoUpdatesChannel")
            .expect("auto-update channel row should exist");
        assert_eq!(row.display_value(), "disabled");
    }

    #[test]
    fn streaming_text_display_row_reads_the_setting_and_defaults_to_character() {
        let config = GlobalConfig::default();
        let mut settings = SettingsJson::default();
        let row = |settings: &SettingsJson| {
            build_items(&config, settings)
                .into_iter()
                .find(|item| item.id == "streamingTextDisplay")
                .expect("streaming text row should exist")
        };
        assert_eq!(row(&settings).display_value(), "character");
        settings.streaming_text_display = Some("line".to_string());
        assert_eq!(row(&settings).display_value(), "line");
        // Unknown values fall back to the default rather than hiding the row.
        settings.streaming_text_display = Some("words".to_string());
        assert_eq!(row(&settings).display_value(), "character");
        // Enter cycles through both modes.
        let mut item = row(&settings);
        item.toggle();
        assert_eq!(item.display_value(), "line");
        item.toggle();
        assert_eq!(item.display_value(), "character");
    }
}
