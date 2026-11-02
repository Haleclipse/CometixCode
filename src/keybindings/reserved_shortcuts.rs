//! Maps to: CC `keybindings/reservedShortcuts.ts`.

use super::types::KeybindingWarningSeverity;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReservedShortcut {
    pub key: &'static str,
    pub reason: &'static str,
    pub severity: KeybindingWarningSeverity,
}

/// Shortcuts that cannot be rebound because CC handles them specially.
pub const NON_REBINDABLE: &[ReservedShortcut] = &[
    ReservedShortcut {
        key: "ctrl+c",
        reason: "Cannot be rebound - used for interrupt/exit (hardcoded)",
        severity: KeybindingWarningSeverity::Error,
    },
    ReservedShortcut {
        key: "ctrl+d",
        reason: "Cannot be rebound - used for exit (hardcoded)",
        severity: KeybindingWarningSeverity::Error,
    },
    ReservedShortcut {
        key: "ctrl+m",
        reason: "Cannot be rebound - identical to Enter in terminals (both send CR)",
        severity: KeybindingWarningSeverity::Error,
    },
];

/// Terminal control shortcuts intercepted before they reach the app.
pub const TERMINAL_RESERVED: &[ReservedShortcut] = &[
    ReservedShortcut {
        key: "ctrl+z",
        reason: "Unix process suspend (SIGTSTP)",
        severity: KeybindingWarningSeverity::Warning,
    },
    ReservedShortcut {
        key: "ctrl+\\",
        reason: "Terminal quit signal (SIGQUIT)",
        severity: KeybindingWarningSeverity::Error,
    },
];

/// macOS shortcuts normally intercepted by the operating system.
pub const MACOS_RESERVED: &[ReservedShortcut] = &[
    ReservedShortcut {
        key: "cmd+c",
        reason: "macOS system copy",
        severity: KeybindingWarningSeverity::Error,
    },
    ReservedShortcut {
        key: "cmd+v",
        reason: "macOS system paste",
        severity: KeybindingWarningSeverity::Error,
    },
    ReservedShortcut {
        key: "cmd+x",
        reason: "macOS system cut",
        severity: KeybindingWarningSeverity::Error,
    },
    ReservedShortcut {
        key: "cmd+q",
        reason: "macOS quit application",
        severity: KeybindingWarningSeverity::Error,
    },
    ReservedShortcut {
        key: "cmd+w",
        reason: "macOS close window/tab",
        severity: KeybindingWarningSeverity::Error,
    },
    ReservedShortcut {
        key: "cmd+tab",
        reason: "macOS app switcher",
        severity: KeybindingWarningSeverity::Error,
    },
    ReservedShortcut {
        key: "cmd+space",
        reason: "macOS Spotlight",
        severity: KeybindingWarningSeverity::Error,
    },
];

/// Maps to CC `getReservedShortcuts()` and preserves source ordering.
pub fn get_reserved_shortcuts() -> Vec<ReservedShortcut> {
    let mut shortcuts = Vec::with_capacity(
        NON_REBINDABLE.len()
            + TERMINAL_RESERVED.len()
            + if cfg!(target_os = "macos") {
                MACOS_RESERVED.len()
            } else {
                0
            },
    );
    shortcuts.extend_from_slice(NON_REBINDABLE);
    shortcuts.extend_from_slice(TERMINAL_RESERVED);
    if cfg!(target_os = "macos") {
        shortcuts.extend_from_slice(MACOS_RESERVED);
    }
    shortcuts
}

/// Maps to CC `normalizeKeyForComparison()`: normalize every chord step,
/// canonicalize modifier aliases, sort modifiers, then rejoin with spaces.
pub fn normalize_key_for_comparison(key: &str) -> String {
    key.split_whitespace()
        .map(normalize_step)
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_step(step: &str) -> String {
    let mut modifiers: Vec<String> = Vec::new();
    let mut main_key = String::new();
    for part in step.split('+') {
        let lower = part.trim().to_ascii_lowercase();
        match lower.as_str() {
            "ctrl" | "control" => modifiers.push("ctrl".to_string()),
            "alt" | "opt" | "option" => modifiers.push("alt".to_string()),
            "meta" => modifiers.push("meta".to_string()),
            "cmd" | "command" => modifiers.push("cmd".to_string()),
            "shift" => modifiers.push("shift".to_string()),
            _ => main_key = lower,
        }
    }
    modifiers.sort_unstable();
    modifiers.push(main_key);
    modifiers.join("+")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_shortcut_tables_match_cc_reasons_severity_and_order() {
        let shortcuts = get_reserved_shortcuts();
        assert_eq!(shortcuts[0], NON_REBINDABLE[0]);
        assert_eq!(shortcuts[3], TERMINAL_RESERVED[0]);
        assert_eq!(NON_REBINDABLE[2].key, "ctrl+m");
        assert_eq!(
            TERMINAL_RESERVED[0].severity,
            KeybindingWarningSeverity::Warning
        );
        assert_eq!(
            TERMINAL_RESERVED[1].severity,
            KeybindingWarningSeverity::Error
        );
        if cfg!(target_os = "macos") {
            assert_eq!(shortcuts.last(), MACOS_RESERVED.last());
        }
    }

    #[test]
    fn normalization_matches_alias_sorting_and_per_step_chords() {
        assert_eq!(
            normalize_key_for_comparison("shift+control+K"),
            "ctrl+shift+k"
        );
        assert_eq!(normalize_key_for_comparison("command+c"), "cmd+c");
        assert_eq!(
            normalize_key_for_comparison("ctrl+x shift+control+B"),
            "ctrl+x ctrl+shift+b"
        );
    }
}
