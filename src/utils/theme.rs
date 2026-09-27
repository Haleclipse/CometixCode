//! Maps to: CC utils/theme.ts — complete 6-variant color system.
//! 1:1 port of all 69 theme color keys from CC's Theme type.
//! Every field, every variant, every RGB value matches the CC source.
//! Source: rebuild/src/utils/theme.ts lines 4-89 (type), 115-596 (6 themes).

use iocraft::Color;

/// Complete theme color palette — maps to CC Theme type (theme.ts:4-89).
/// All 69 keys ported verbatim.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub auto_accept: Color,
    pub bash_border: Color,
    pub claude: Color,
    pub claude_shimmer: Color,
    pub claude_blue: Color,
    pub claude_blue_shimmer: Color,
    pub permission: Color,
    pub permission_shimmer: Color,
    pub plan_mode: Color,
    pub ide: Color,
    pub prompt_border: Color,
    pub prompt_border_shimmer: Color,
    pub text: Color,
    pub inverse_text: Color,
    pub inactive: Color,
    pub inactive_shimmer: Color,
    pub subtle: Color,
    pub suggestion: Color,
    pub remember: Color,
    pub background: Color,
    pub success: Color,
    pub error: Color,
    pub warning: Color,
    pub merged: Color,
    pub warning_shimmer: Color,
    pub diff_added: Color,
    pub diff_removed: Color,
    pub diff_added_dimmed: Color,
    pub diff_removed_dimmed: Color,
    pub diff_added_word: Color,
    pub diff_removed_word: Color,
    pub agent_red: Color,
    pub agent_blue: Color,
    pub agent_green: Color,
    pub agent_yellow: Color,
    pub agent_purple: Color,
    pub agent_orange: Color,
    pub agent_pink: Color,
    pub agent_cyan: Color,
    pub professional_blue: Color,
    pub chrome_yellow: Color,
    pub clawd_body: Color,
    pub clawd_bg: Color,
    pub user_message_bg: Color,
    pub user_message_bg_hover: Color,
    pub message_actions_bg: Color,
    pub selection_bg: Color,
    pub bash_message_bg: Color,
    pub memory_bg: Color,
    pub rate_limit_fill: Color,
    pub rate_limit_empty: Color,
    pub fast_mode: Color,
    pub fast_mode_shimmer: Color,
    pub brief_label_you: Color,
    pub brief_label_claude: Color,
    pub rainbow_red: Color,
    pub rainbow_orange: Color,
    pub rainbow_yellow: Color,
    pub rainbow_green: Color,
    pub rainbow_blue: Color,
    pub rainbow_indigo: Color,
    pub rainbow_violet: Color,
    pub rainbow_red_shimmer: Color,
    pub rainbow_orange_shimmer: Color,
    pub rainbow_yellow_shimmer: Color,
    pub rainbow_green_shimmer: Color,
    pub rainbow_blue_shimmer: Color,
    pub rainbow_indigo_shimmer: Color,
    pub rainbow_violet_shimmer: Color,
}

/// Names every official `Theme` color key so Rust callers can resolve colors
/// through the same key-based seam as CC's `ThemedText` / `ThemedBox`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeColorKey {
    AutoAccept,
    BashBorder,
    Claude,
    ClaudeShimmer,
    ClaudeBlue,
    ClaudeBlueShimmer,
    Permission,
    PermissionShimmer,
    PlanMode,
    Ide,
    PromptBorder,
    PromptBorderShimmer,
    Text,
    InverseText,
    Inactive,
    InactiveShimmer,
    Subtle,
    Suggestion,
    Remember,
    Background,
    Success,
    Error,
    Warning,
    Merged,
    WarningShimmer,
    DiffAdded,
    DiffRemoved,
    DiffAddedDimmed,
    DiffRemovedDimmed,
    DiffAddedWord,
    DiffRemovedWord,
    AgentRed,
    AgentBlue,
    AgentGreen,
    AgentYellow,
    AgentPurple,
    AgentOrange,
    AgentPink,
    AgentCyan,
    ProfessionalBlue,
    ChromeYellow,
    ClawdBody,
    ClawdBg,
    UserMessageBg,
    UserMessageBgHover,
    MessageActionsBg,
    SelectionBg,
    BashMessageBg,
    MemoryBg,
    RateLimitFill,
    RateLimitEmpty,
    FastMode,
    FastModeShimmer,
    BriefLabelYou,
    BriefLabelClaude,
    RainbowRed,
    RainbowOrange,
    RainbowYellow,
    RainbowGreen,
    RainbowBlue,
    RainbowIndigo,
    RainbowViolet,
    RainbowRedShimmer,
    RainbowOrangeShimmer,
    RainbowYellowShimmer,
    RainbowGreenShimmer,
    RainbowBlueShimmer,
    RainbowIndigoShimmer,
    RainbowVioletShimmer,
}

impl ThemeColorKey {
    pub const ALL: [Self; 69] = [
        Self::AutoAccept,
        Self::BashBorder,
        Self::Claude,
        Self::ClaudeShimmer,
        Self::ClaudeBlue,
        Self::ClaudeBlueShimmer,
        Self::Permission,
        Self::PermissionShimmer,
        Self::PlanMode,
        Self::Ide,
        Self::PromptBorder,
        Self::PromptBorderShimmer,
        Self::Text,
        Self::InverseText,
        Self::Inactive,
        Self::InactiveShimmer,
        Self::Subtle,
        Self::Suggestion,
        Self::Remember,
        Self::Background,
        Self::Success,
        Self::Error,
        Self::Warning,
        Self::Merged,
        Self::WarningShimmer,
        Self::DiffAdded,
        Self::DiffRemoved,
        Self::DiffAddedDimmed,
        Self::DiffRemovedDimmed,
        Self::DiffAddedWord,
        Self::DiffRemovedWord,
        Self::AgentRed,
        Self::AgentBlue,
        Self::AgentGreen,
        Self::AgentYellow,
        Self::AgentPurple,
        Self::AgentOrange,
        Self::AgentPink,
        Self::AgentCyan,
        Self::ProfessionalBlue,
        Self::ChromeYellow,
        Self::ClawdBody,
        Self::ClawdBg,
        Self::UserMessageBg,
        Self::UserMessageBgHover,
        Self::MessageActionsBg,
        Self::SelectionBg,
        Self::BashMessageBg,
        Self::MemoryBg,
        Self::RateLimitFill,
        Self::RateLimitEmpty,
        Self::FastMode,
        Self::FastModeShimmer,
        Self::BriefLabelYou,
        Self::BriefLabelClaude,
        Self::RainbowRed,
        Self::RainbowOrange,
        Self::RainbowYellow,
        Self::RainbowGreen,
        Self::RainbowBlue,
        Self::RainbowIndigo,
        Self::RainbowViolet,
        Self::RainbowRedShimmer,
        Self::RainbowOrangeShimmer,
        Self::RainbowYellowShimmer,
        Self::RainbowGreenShimmer,
        Self::RainbowBlueShimmer,
        Self::RainbowIndigoShimmer,
        Self::RainbowVioletShimmer,
    ];

    pub fn official_key(self) -> &'static str {
        match self {
            Self::AutoAccept => "autoAccept",
            Self::BashBorder => "bashBorder",
            Self::Claude => "claude",
            Self::ClaudeShimmer => "claudeShimmer",
            Self::ClaudeBlue => "claudeBlue_FOR_SYSTEM_SPINNER",
            Self::ClaudeBlueShimmer => "claudeBlueShimmer_FOR_SYSTEM_SPINNER",
            Self::Permission => "permission",
            Self::PermissionShimmer => "permissionShimmer",
            Self::PlanMode => "planMode",
            Self::Ide => "ide",
            Self::PromptBorder => "promptBorder",
            Self::PromptBorderShimmer => "promptBorderShimmer",
            Self::Text => "text",
            Self::InverseText => "inverseText",
            Self::Inactive => "inactive",
            Self::InactiveShimmer => "inactiveShimmer",
            Self::Subtle => "subtle",
            Self::Suggestion => "suggestion",
            Self::Remember => "remember",
            Self::Background => "background",
            Self::Success => "success",
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Merged => "merged",
            Self::WarningShimmer => "warningShimmer",
            Self::DiffAdded => "diffAdded",
            Self::DiffRemoved => "diffRemoved",
            Self::DiffAddedDimmed => "diffAddedDimmed",
            Self::DiffRemovedDimmed => "diffRemovedDimmed",
            Self::DiffAddedWord => "diffAddedWord",
            Self::DiffRemovedWord => "diffRemovedWord",
            Self::AgentRed => "red_FOR_SUBAGENTS_ONLY",
            Self::AgentBlue => "blue_FOR_SUBAGENTS_ONLY",
            Self::AgentGreen => "green_FOR_SUBAGENTS_ONLY",
            Self::AgentYellow => "yellow_FOR_SUBAGENTS_ONLY",
            Self::AgentPurple => "purple_FOR_SUBAGENTS_ONLY",
            Self::AgentOrange => "orange_FOR_SUBAGENTS_ONLY",
            Self::AgentPink => "pink_FOR_SUBAGENTS_ONLY",
            Self::AgentCyan => "cyan_FOR_SUBAGENTS_ONLY",
            Self::ProfessionalBlue => "professionalBlue",
            Self::ChromeYellow => "chromeYellow",
            Self::ClawdBody => "clawd_body",
            Self::ClawdBg => "clawd_background",
            Self::UserMessageBg => "userMessageBackground",
            Self::UserMessageBgHover => "userMessageBackgroundHover",
            Self::MessageActionsBg => "messageActionsBackground",
            Self::SelectionBg => "selectionBg",
            Self::BashMessageBg => "bashMessageBackgroundColor",
            Self::MemoryBg => "memoryBackgroundColor",
            Self::RateLimitFill => "rate_limit_fill",
            Self::RateLimitEmpty => "rate_limit_empty",
            Self::FastMode => "fastMode",
            Self::FastModeShimmer => "fastModeShimmer",
            Self::BriefLabelYou => "briefLabelYou",
            Self::BriefLabelClaude => "briefLabelClaude",
            Self::RainbowRed => "rainbow_red",
            Self::RainbowOrange => "rainbow_orange",
            Self::RainbowYellow => "rainbow_yellow",
            Self::RainbowGreen => "rainbow_green",
            Self::RainbowBlue => "rainbow_blue",
            Self::RainbowIndigo => "rainbow_indigo",
            Self::RainbowViolet => "rainbow_violet",
            Self::RainbowRedShimmer => "rainbow_red_shimmer",
            Self::RainbowOrangeShimmer => "rainbow_orange_shimmer",
            Self::RainbowYellowShimmer => "rainbow_yellow_shimmer",
            Self::RainbowGreenShimmer => "rainbow_green_shimmer",
            Self::RainbowBlueShimmer => "rainbow_blue_shimmer",
            Self::RainbowIndigoShimmer => "rainbow_indigo_shimmer",
            Self::RainbowVioletShimmer => "rainbow_violet_shimmer",
        }
    }
}

impl Theme {
    pub fn color(self, key: ThemeColorKey) -> Color {
        match key {
            ThemeColorKey::AutoAccept => self.auto_accept,
            ThemeColorKey::BashBorder => self.bash_border,
            ThemeColorKey::Claude => self.claude,
            ThemeColorKey::ClaudeShimmer => self.claude_shimmer,
            ThemeColorKey::ClaudeBlue => self.claude_blue,
            ThemeColorKey::ClaudeBlueShimmer => self.claude_blue_shimmer,
            ThemeColorKey::Permission => self.permission,
            ThemeColorKey::PermissionShimmer => self.permission_shimmer,
            ThemeColorKey::PlanMode => self.plan_mode,
            ThemeColorKey::Ide => self.ide,
            ThemeColorKey::PromptBorder => self.prompt_border,
            ThemeColorKey::PromptBorderShimmer => self.prompt_border_shimmer,
            ThemeColorKey::Text => self.text,
            ThemeColorKey::InverseText => self.inverse_text,
            ThemeColorKey::Inactive => self.inactive,
            ThemeColorKey::InactiveShimmer => self.inactive_shimmer,
            ThemeColorKey::Subtle => self.subtle,
            ThemeColorKey::Suggestion => self.suggestion,
            ThemeColorKey::Remember => self.remember,
            ThemeColorKey::Background => self.background,
            ThemeColorKey::Success => self.success,
            ThemeColorKey::Error => self.error,
            ThemeColorKey::Warning => self.warning,
            ThemeColorKey::Merged => self.merged,
            ThemeColorKey::WarningShimmer => self.warning_shimmer,
            ThemeColorKey::DiffAdded => self.diff_added,
            ThemeColorKey::DiffRemoved => self.diff_removed,
            ThemeColorKey::DiffAddedDimmed => self.diff_added_dimmed,
            ThemeColorKey::DiffRemovedDimmed => self.diff_removed_dimmed,
            ThemeColorKey::DiffAddedWord => self.diff_added_word,
            ThemeColorKey::DiffRemovedWord => self.diff_removed_word,
            ThemeColorKey::AgentRed => self.agent_red,
            ThemeColorKey::AgentBlue => self.agent_blue,
            ThemeColorKey::AgentGreen => self.agent_green,
            ThemeColorKey::AgentYellow => self.agent_yellow,
            ThemeColorKey::AgentPurple => self.agent_purple,
            ThemeColorKey::AgentOrange => self.agent_orange,
            ThemeColorKey::AgentPink => self.agent_pink,
            ThemeColorKey::AgentCyan => self.agent_cyan,
            ThemeColorKey::ProfessionalBlue => self.professional_blue,
            ThemeColorKey::ChromeYellow => self.chrome_yellow,
            ThemeColorKey::ClawdBody => self.clawd_body,
            ThemeColorKey::ClawdBg => self.clawd_bg,
            ThemeColorKey::UserMessageBg => self.user_message_bg,
            ThemeColorKey::UserMessageBgHover => self.user_message_bg_hover,
            ThemeColorKey::MessageActionsBg => self.message_actions_bg,
            ThemeColorKey::SelectionBg => self.selection_bg,
            ThemeColorKey::BashMessageBg => self.bash_message_bg,
            ThemeColorKey::MemoryBg => self.memory_bg,
            ThemeColorKey::RateLimitFill => self.rate_limit_fill,
            ThemeColorKey::RateLimitEmpty => self.rate_limit_empty,
            ThemeColorKey::FastMode => self.fast_mode,
            ThemeColorKey::FastModeShimmer => self.fast_mode_shimmer,
            ThemeColorKey::BriefLabelYou => self.brief_label_you,
            ThemeColorKey::BriefLabelClaude => self.brief_label_claude,
            ThemeColorKey::RainbowRed => self.rainbow_red,
            ThemeColorKey::RainbowOrange => self.rainbow_orange,
            ThemeColorKey::RainbowYellow => self.rainbow_yellow,
            ThemeColorKey::RainbowGreen => self.rainbow_green,
            ThemeColorKey::RainbowBlue => self.rainbow_blue,
            ThemeColorKey::RainbowIndigo => self.rainbow_indigo,
            ThemeColorKey::RainbowViolet => self.rainbow_violet,
            ThemeColorKey::RainbowRedShimmer => self.rainbow_red_shimmer,
            ThemeColorKey::RainbowOrangeShimmer => self.rainbow_orange_shimmer,
            ThemeColorKey::RainbowYellowShimmer => self.rainbow_yellow_shimmer,
            ThemeColorKey::RainbowGreenShimmer => self.rainbow_green_shimmer,
            ThemeColorKey::RainbowBlueShimmer => self.rainbow_blue_shimmer,
            ThemeColorKey::RainbowIndigoShimmer => self.rainbow_indigo_shimmer,
            ThemeColorKey::RainbowVioletShimmer => self.rainbow_violet_shimmer,
        }
    }

    pub fn palette(self) -> [Color; 69] {
        ThemeColorKey::ALL.map(|key| self.color(key))
    }
}

// Helper macro to reduce rgb boilerplate
macro_rules! rgb {
    ($r:expr, $g:expr, $b:expr) => {
        Color::Rgb {
            r: $r,
            g: $g,
            b: $b,
        }
    };
}

// ══════════════════════════════════════════════════════════════════
// Dark theme — maps to CC darkTheme (theme.ts:440-515)
// ══════════════════════════════════════════════════════════════════
pub static DARK: Theme = Theme {
    auto_accept: rgb!(175, 135, 255),
    bash_border: rgb!(253, 93, 177),
    claude: rgb!(215, 119, 87),
    claude_shimmer: rgb!(235, 159, 127),
    claude_blue: rgb!(147, 165, 255),
    claude_blue_shimmer: rgb!(177, 195, 255),
    permission: rgb!(177, 185, 249),
    permission_shimmer: rgb!(207, 215, 255),
    plan_mode: rgb!(72, 150, 140),
    ide: rgb!(71, 130, 200),
    prompt_border: rgb!(136, 136, 136),
    prompt_border_shimmer: rgb!(166, 166, 166),
    text: rgb!(255, 255, 255),
    inverse_text: rgb!(0, 0, 0),
    inactive: rgb!(153, 153, 153),
    inactive_shimmer: rgb!(193, 193, 193),
    subtle: rgb!(80, 80, 80),
    suggestion: rgb!(177, 185, 249),
    remember: rgb!(177, 185, 249),
    background: rgb!(0, 204, 204),
    success: rgb!(78, 186, 101),
    error: rgb!(255, 107, 128),
    warning: rgb!(255, 193, 7),
    merged: rgb!(175, 135, 255),
    warning_shimmer: rgb!(255, 223, 57),
    diff_added: rgb!(34, 92, 43),
    diff_removed: rgb!(122, 41, 54),
    diff_added_dimmed: rgb!(71, 88, 74),
    diff_removed_dimmed: rgb!(105, 72, 77),
    diff_added_word: rgb!(56, 166, 96),
    diff_removed_word: rgb!(179, 89, 107),
    agent_red: rgb!(220, 38, 38),
    agent_blue: rgb!(37, 99, 235),
    agent_green: rgb!(22, 163, 74),
    agent_yellow: rgb!(202, 138, 4),
    agent_purple: rgb!(147, 51, 234),
    agent_orange: rgb!(234, 88, 12),
    agent_pink: rgb!(219, 39, 119),
    agent_cyan: rgb!(8, 145, 178),
    professional_blue: rgb!(106, 155, 204),
    chrome_yellow: rgb!(251, 188, 4),
    clawd_body: rgb!(215, 119, 87),
    clawd_bg: rgb!(0, 0, 0),
    user_message_bg: rgb!(55, 55, 55),
    user_message_bg_hover: rgb!(70, 70, 70),
    message_actions_bg: rgb!(44, 50, 62),
    selection_bg: rgb!(38, 79, 120),
    bash_message_bg: rgb!(65, 60, 65),
    memory_bg: rgb!(55, 65, 70),
    rate_limit_fill: rgb!(177, 185, 249),
    rate_limit_empty: rgb!(80, 83, 112),
    fast_mode: rgb!(255, 120, 20),
    fast_mode_shimmer: rgb!(255, 165, 70),
    brief_label_you: rgb!(122, 180, 232),
    brief_label_claude: rgb!(215, 119, 87),
    rainbow_red: rgb!(235, 95, 87),
    rainbow_orange: rgb!(245, 139, 87),
    rainbow_yellow: rgb!(250, 195, 95),
    rainbow_green: rgb!(145, 200, 130),
    rainbow_blue: rgb!(130, 170, 220),
    rainbow_indigo: rgb!(155, 130, 200),
    rainbow_violet: rgb!(200, 130, 180),
    rainbow_red_shimmer: rgb!(250, 155, 147),
    rainbow_orange_shimmer: rgb!(255, 185, 137),
    rainbow_yellow_shimmer: rgb!(255, 225, 155),
    rainbow_green_shimmer: rgb!(185, 230, 180),
    rainbow_blue_shimmer: rgb!(180, 205, 240),
    rainbow_indigo_shimmer: rgb!(195, 180, 230),
    rainbow_violet_shimmer: rgb!(230, 180, 210),
};

// ══════════════════════════════════════════════════════════════════
// Light theme — maps to CC lightTheme (theme.ts:115-191)
// ══════════════════════════════════════════════════════════════════
pub static LIGHT: Theme = Theme {
    auto_accept: rgb!(135, 0, 255),
    bash_border: rgb!(255, 0, 135),
    claude: rgb!(215, 119, 87),
    claude_shimmer: rgb!(245, 149, 117),
    claude_blue: rgb!(87, 105, 247),
    claude_blue_shimmer: rgb!(117, 135, 255),
    permission: rgb!(87, 105, 247),
    permission_shimmer: rgb!(137, 155, 255),
    plan_mode: rgb!(0, 102, 102),
    ide: rgb!(71, 130, 200),
    prompt_border: rgb!(153, 153, 153),
    prompt_border_shimmer: rgb!(183, 183, 183),
    text: rgb!(0, 0, 0),
    inverse_text: rgb!(255, 255, 255),
    inactive: rgb!(102, 102, 102),
    inactive_shimmer: rgb!(142, 142, 142),
    subtle: rgb!(175, 175, 175),
    suggestion: rgb!(87, 105, 247),
    remember: rgb!(0, 0, 255),
    background: rgb!(0, 153, 153),
    success: rgb!(44, 122, 57),
    error: rgb!(171, 43, 63),
    warning: rgb!(150, 108, 30),
    merged: rgb!(135, 0, 255),
    warning_shimmer: rgb!(200, 158, 80),
    diff_added: rgb!(105, 219, 124),
    diff_removed: rgb!(255, 168, 180),
    diff_added_dimmed: rgb!(199, 225, 203),
    diff_removed_dimmed: rgb!(253, 210, 216),
    diff_added_word: rgb!(47, 157, 68),
    diff_removed_word: rgb!(209, 69, 75),
    agent_red: rgb!(220, 38, 38),
    agent_blue: rgb!(37, 99, 235),
    agent_green: rgb!(22, 163, 74),
    agent_yellow: rgb!(202, 138, 4),
    agent_purple: rgb!(147, 51, 234),
    agent_orange: rgb!(234, 88, 12),
    agent_pink: rgb!(219, 39, 119),
    agent_cyan: rgb!(8, 145, 178),
    professional_blue: rgb!(106, 155, 204),
    chrome_yellow: rgb!(251, 188, 4),
    clawd_body: rgb!(215, 119, 87),
    clawd_bg: rgb!(0, 0, 0),
    user_message_bg: rgb!(240, 240, 240),
    user_message_bg_hover: rgb!(252, 252, 252),
    message_actions_bg: rgb!(232, 236, 244),
    selection_bg: rgb!(180, 213, 255),
    bash_message_bg: rgb!(250, 245, 250),
    memory_bg: rgb!(230, 245, 250),
    rate_limit_fill: rgb!(87, 105, 247),
    rate_limit_empty: rgb!(39, 47, 111),
    fast_mode: rgb!(255, 106, 0),
    fast_mode_shimmer: rgb!(255, 150, 50),
    brief_label_you: rgb!(37, 99, 235),
    brief_label_claude: rgb!(215, 119, 87),
    rainbow_red: rgb!(235, 95, 87),
    rainbow_orange: rgb!(245, 139, 87),
    rainbow_yellow: rgb!(250, 195, 95),
    rainbow_green: rgb!(145, 200, 130),
    rainbow_blue: rgb!(130, 170, 220),
    rainbow_indigo: rgb!(155, 130, 200),
    rainbow_violet: rgb!(200, 130, 180),
    rainbow_red_shimmer: rgb!(250, 155, 147),
    rainbow_orange_shimmer: rgb!(255, 185, 137),
    rainbow_yellow_shimmer: rgb!(255, 225, 155),
    rainbow_green_shimmer: rgb!(185, 230, 180),
    rainbow_blue_shimmer: rgb!(180, 205, 240),
    rainbow_indigo_shimmer: rgb!(195, 180, 230),
    rainbow_violet_shimmer: rgb!(230, 180, 210),
};

// ══════════════════════════════════════════════════════════════════
// Dark ANSI theme — maps to CC darkAnsiTheme (theme.ts:278-353)
// ══════════════════════════════════════════════════════════════════
pub static DARK_ANSI: Theme = Theme {
    auto_accept: Color::Magenta,
    bash_border: Color::Magenta,
    claude: Color::Red,
    claude_shimmer: Color::Yellow,
    claude_blue: Color::Blue,
    claude_blue_shimmer: Color::Blue,
    permission: Color::Blue,
    permission_shimmer: Color::Blue,
    plan_mode: Color::Cyan,
    ide: Color::DarkBlue,
    prompt_border: Color::Grey,
    prompt_border_shimmer: Color::White,
    text: Color::White,
    inverse_text: Color::Black,
    inactive: Color::Grey,
    inactive_shimmer: Color::White,
    subtle: Color::Grey,
    suggestion: Color::Blue,
    remember: Color::Blue,
    background: Color::Cyan,
    success: Color::Green,
    error: Color::Red,
    warning: Color::Yellow,
    merged: Color::Magenta,
    warning_shimmer: Color::Yellow,
    diff_added: Color::DarkGreen,
    diff_removed: Color::DarkRed,
    diff_added_dimmed: Color::DarkGreen,
    diff_removed_dimmed: Color::DarkRed,
    diff_added_word: Color::Green,
    diff_removed_word: Color::Red,
    agent_red: Color::Red,
    agent_blue: Color::Blue,
    agent_green: Color::Green,
    agent_yellow: Color::Yellow,
    agent_purple: Color::Magenta,
    agent_orange: Color::Red,
    agent_pink: Color::Magenta,
    agent_cyan: Color::Cyan,
    professional_blue: rgb!(106, 155, 204),
    chrome_yellow: Color::Yellow,
    clawd_body: Color::Red,
    clawd_bg: Color::Black,
    user_message_bg: Color::DarkGrey,
    user_message_bg_hover: Color::Grey,
    message_actions_bg: Color::DarkGrey,
    selection_bg: Color::DarkBlue,
    bash_message_bg: Color::Black,
    memory_bg: Color::DarkGrey,
    rate_limit_fill: Color::DarkYellow,
    rate_limit_empty: Color::Grey,
    fast_mode: Color::Red,
    fast_mode_shimmer: Color::Red,
    brief_label_you: Color::Blue,
    brief_label_claude: Color::Red,
    rainbow_red: Color::DarkRed,
    rainbow_orange: Color::Red,
    rainbow_yellow: Color::DarkYellow,
    rainbow_green: Color::DarkGreen,
    rainbow_blue: Color::DarkCyan,
    rainbow_indigo: Color::DarkBlue,
    rainbow_violet: Color::DarkMagenta,
    rainbow_red_shimmer: Color::Red,
    rainbow_orange_shimmer: Color::DarkYellow,
    rainbow_yellow_shimmer: Color::Yellow,
    rainbow_green_shimmer: Color::Green,
    rainbow_blue_shimmer: Color::Cyan,
    rainbow_indigo_shimmer: Color::Blue,
    rainbow_violet_shimmer: Color::Magenta,
};

// ══════════════════════════════════════════════════════════════════
// Light ANSI theme — maps to CC lightAnsiTheme (theme.ts:197-272)
// ══════════════════════════════════════════════════════════════════
pub static LIGHT_ANSI: Theme = Theme {
    auto_accept: Color::DarkMagenta,
    bash_border: Color::DarkMagenta,
    claude: Color::Red,
    claude_shimmer: Color::Yellow,
    claude_blue: Color::DarkBlue,
    claude_blue_shimmer: Color::Blue,
    permission: Color::DarkBlue,
    permission_shimmer: Color::Blue,
    plan_mode: Color::DarkCyan,
    ide: Color::Blue,
    prompt_border: Color::Grey,
    prompt_border_shimmer: Color::White,
    text: Color::Black,
    inverse_text: Color::Grey,
    inactive: Color::DarkGrey,
    inactive_shimmer: Color::Grey,
    subtle: Color::DarkGrey,
    suggestion: Color::DarkBlue,
    remember: Color::DarkBlue,
    background: Color::DarkCyan,
    success: Color::DarkGreen,
    error: Color::DarkRed,
    warning: Color::DarkYellow,
    merged: Color::DarkMagenta,
    warning_shimmer: Color::Yellow,
    diff_added: Color::DarkGreen,
    diff_removed: Color::DarkRed,
    diff_added_dimmed: Color::DarkGreen,
    diff_removed_dimmed: Color::DarkRed,
    diff_added_word: Color::Green,
    diff_removed_word: Color::Red,
    agent_red: Color::DarkRed,
    agent_blue: Color::DarkBlue,
    agent_green: Color::DarkGreen,
    agent_yellow: Color::DarkYellow,
    agent_purple: Color::DarkMagenta,
    agent_orange: Color::Red,
    agent_pink: Color::Magenta,
    agent_cyan: Color::DarkCyan,
    professional_blue: Color::Blue,
    chrome_yellow: Color::DarkYellow,
    clawd_body: Color::Red,
    clawd_bg: Color::Black,
    user_message_bg: Color::Grey,
    user_message_bg_hover: Color::White,
    message_actions_bg: Color::Grey,
    selection_bg: Color::DarkCyan,
    bash_message_bg: Color::White,
    memory_bg: Color::Grey,
    rate_limit_fill: Color::DarkYellow,
    rate_limit_empty: Color::Black,
    fast_mode: Color::DarkRed,
    fast_mode_shimmer: Color::Red,
    brief_label_you: Color::DarkBlue,
    brief_label_claude: Color::Red,
    rainbow_red: Color::DarkRed,
    rainbow_orange: Color::Red,
    rainbow_yellow: Color::DarkYellow,
    rainbow_green: Color::DarkGreen,
    rainbow_blue: Color::DarkCyan,
    rainbow_indigo: Color::DarkBlue,
    rainbow_violet: Color::DarkMagenta,
    rainbow_red_shimmer: Color::Red,
    rainbow_orange_shimmer: Color::DarkYellow,
    rainbow_yellow_shimmer: Color::Yellow,
    rainbow_green_shimmer: Color::Green,
    rainbow_blue_shimmer: Color::Cyan,
    rainbow_indigo_shimmer: Color::Blue,
    rainbow_violet_shimmer: Color::Magenta,
};

// ══════════════════════════════════════════════════════════════════
// Dark daltonized — maps to CC darkDaltonizedTheme (theme.ts:521-596)
// ══════════════════════════════════════════════════════════════════
pub static DARK_DALTONIZED: Theme = Theme {
    auto_accept: rgb!(175, 135, 255),
    bash_border: rgb!(51, 153, 255),
    claude: rgb!(255, 153, 51),
    claude_shimmer: rgb!(255, 183, 101),
    claude_blue: rgb!(153, 204, 255),
    claude_blue_shimmer: rgb!(183, 224, 255),
    permission: rgb!(153, 204, 255),
    permission_shimmer: rgb!(183, 224, 255),
    plan_mode: rgb!(102, 153, 153),
    ide: rgb!(71, 130, 200),
    prompt_border: rgb!(136, 136, 136),
    prompt_border_shimmer: rgb!(166, 166, 166),
    text: rgb!(255, 255, 255),
    inverse_text: rgb!(0, 0, 0),
    inactive: rgb!(153, 153, 153),
    inactive_shimmer: rgb!(193, 193, 193),
    subtle: rgb!(80, 80, 80),
    suggestion: rgb!(153, 204, 255),
    remember: rgb!(153, 204, 255),
    background: rgb!(0, 204, 204),
    success: rgb!(51, 153, 255),
    error: rgb!(255, 102, 102),
    warning: rgb!(255, 204, 0),
    merged: rgb!(175, 135, 255),
    warning_shimmer: rgb!(255, 234, 50),
    diff_added: rgb!(0, 68, 102),
    diff_removed: rgb!(102, 0, 0),
    diff_added_dimmed: rgb!(62, 81, 91),
    diff_removed_dimmed: rgb!(62, 44, 44),
    diff_added_word: rgb!(0, 119, 179),
    diff_removed_word: rgb!(179, 0, 0),
    agent_red: rgb!(255, 102, 102),
    agent_blue: rgb!(102, 178, 255),
    agent_green: rgb!(102, 255, 102),
    agent_yellow: rgb!(255, 255, 102),
    agent_purple: rgb!(178, 102, 255),
    agent_orange: rgb!(255, 178, 102),
    agent_pink: rgb!(255, 153, 204),
    agent_cyan: rgb!(102, 204, 204),
    professional_blue: rgb!(106, 155, 204),
    chrome_yellow: rgb!(251, 188, 4),
    clawd_body: rgb!(215, 119, 87),
    clawd_bg: rgb!(0, 0, 0),
    user_message_bg: rgb!(55, 55, 55),
    user_message_bg_hover: rgb!(70, 70, 70),
    message_actions_bg: rgb!(44, 50, 62),
    selection_bg: rgb!(38, 79, 120),
    bash_message_bg: rgb!(65, 60, 65),
    memory_bg: rgb!(55, 65, 70),
    rate_limit_fill: rgb!(153, 204, 255),
    rate_limit_empty: rgb!(69, 92, 115),
    fast_mode: rgb!(255, 120, 20),
    fast_mode_shimmer: rgb!(255, 165, 70),
    brief_label_you: rgb!(122, 180, 232),
    brief_label_claude: rgb!(255, 153, 51),
    rainbow_red: rgb!(235, 95, 87),
    rainbow_orange: rgb!(245, 139, 87),
    rainbow_yellow: rgb!(250, 195, 95),
    rainbow_green: rgb!(145, 200, 130),
    rainbow_blue: rgb!(130, 170, 220),
    rainbow_indigo: rgb!(155, 130, 200),
    rainbow_violet: rgb!(200, 130, 180),
    rainbow_red_shimmer: rgb!(250, 155, 147),
    rainbow_orange_shimmer: rgb!(255, 185, 137),
    rainbow_yellow_shimmer: rgb!(255, 225, 155),
    rainbow_green_shimmer: rgb!(185, 230, 180),
    rainbow_blue_shimmer: rgb!(180, 205, 240),
    rainbow_indigo_shimmer: rgb!(195, 180, 230),
    rainbow_violet_shimmer: rgb!(230, 180, 210),
};

// ══════════════════════════════════════════════════════════════════
// Light daltonized — maps to CC lightDaltonizedTheme (theme.ts:359-434)
// ══════════════════════════════════════════════════════════════════
pub static LIGHT_DALTONIZED: Theme = Theme {
    auto_accept: rgb!(135, 0, 255),
    bash_border: rgb!(0, 102, 204),
    claude: rgb!(255, 153, 51),
    claude_shimmer: rgb!(255, 183, 101),
    claude_blue: rgb!(51, 102, 255),
    claude_blue_shimmer: rgb!(101, 152, 255),
    permission: rgb!(51, 102, 255),
    permission_shimmer: rgb!(101, 152, 255),
    plan_mode: rgb!(51, 102, 102),
    ide: rgb!(71, 130, 200),
    prompt_border: rgb!(153, 153, 153),
    prompt_border_shimmer: rgb!(183, 183, 183),
    text: rgb!(0, 0, 0),
    inverse_text: rgb!(255, 255, 255),
    inactive: rgb!(102, 102, 102),
    inactive_shimmer: rgb!(142, 142, 142),
    subtle: rgb!(175, 175, 175),
    suggestion: rgb!(51, 102, 255),
    remember: rgb!(51, 102, 255),
    background: rgb!(0, 153, 153),
    success: rgb!(0, 102, 153),
    error: rgb!(204, 0, 0),
    warning: rgb!(255, 153, 0),
    merged: rgb!(135, 0, 255),
    warning_shimmer: rgb!(255, 183, 50),
    diff_added: rgb!(153, 204, 255),
    diff_removed: rgb!(255, 204, 204),
    diff_added_dimmed: rgb!(209, 231, 253),
    diff_removed_dimmed: rgb!(255, 233, 233),
    diff_added_word: rgb!(51, 102, 204),
    diff_removed_word: rgb!(153, 51, 51),
    agent_red: rgb!(204, 0, 0),
    agent_blue: rgb!(0, 102, 204),
    agent_green: rgb!(0, 204, 0),
    agent_yellow: rgb!(255, 204, 0),
    agent_purple: rgb!(128, 0, 128),
    agent_orange: rgb!(255, 128, 0),
    agent_pink: rgb!(255, 102, 178),
    agent_cyan: rgb!(0, 178, 178),
    professional_blue: rgb!(106, 155, 204),
    chrome_yellow: rgb!(251, 188, 4),
    clawd_body: rgb!(215, 119, 87),
    clawd_bg: rgb!(0, 0, 0),
    user_message_bg: rgb!(220, 220, 220),
    user_message_bg_hover: rgb!(232, 232, 232),
    message_actions_bg: rgb!(210, 216, 226),
    selection_bg: rgb!(180, 213, 255),
    bash_message_bg: rgb!(250, 245, 250),
    memory_bg: rgb!(230, 245, 250),
    rate_limit_fill: rgb!(51, 102, 255),
    rate_limit_empty: rgb!(23, 46, 114),
    fast_mode: rgb!(255, 106, 0),
    fast_mode_shimmer: rgb!(255, 150, 50),
    brief_label_you: rgb!(37, 99, 235),
    brief_label_claude: rgb!(255, 153, 51),
    rainbow_red: rgb!(235, 95, 87),
    rainbow_orange: rgb!(245, 139, 87),
    rainbow_yellow: rgb!(250, 195, 95),
    rainbow_green: rgb!(145, 200, 130),
    rainbow_blue: rgb!(130, 170, 220),
    rainbow_indigo: rgb!(155, 130, 200),
    rainbow_violet: rgb!(200, 130, 180),
    rainbow_red_shimmer: rgb!(250, 155, 147),
    rainbow_orange_shimmer: rgb!(255, 185, 137),
    rainbow_yellow_shimmer: rgb!(255, 225, 155),
    rainbow_green_shimmer: rgb!(185, 230, 180),
    rainbow_blue_shimmer: rgb!(180, 205, 240),
    rainbow_indigo_shimmer: rgb!(195, 180, 230),
    rainbow_violet_shimmer: rgb!(230, 180, 210),
};

// ══════════════════════════════════════════════════════════════════
// Theme name + selection
// ══════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeName {
    /// CC `DEFAULT_THEME` (`ThemeProvider.tsx:28`).
    #[default]
    Dark,
    Light,
    DarkAnsi,
    LightAnsi,
    DarkDaltonized,
    LightDaltonized,
}

/// Official ThemePicker order from `components/ThemePicker.tsx` when
/// `AUTO_THEME` is not enabled in an external build.
pub const THEME_PICKER_ORDER: [ThemeName; 6] = [
    ThemeName::Dark,
    ThemeName::Light,
    ThemeName::DarkDaltonized,
    ThemeName::LightDaltonized,
    ThemeName::DarkAnsi,
    ThemeName::LightAnsi,
];

impl ThemeName {
    pub fn setting_value(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
            Self::DarkAnsi => "dark-ansi",
            Self::LightAnsi => "light-ansi",
            Self::DarkDaltonized => "dark-daltonized",
            Self::LightDaltonized => "light-daltonized",
        }
    }

    pub fn display_label(self) -> &'static str {
        match self {
            Self::Dark => "Dark mode",
            Self::Light => "Light mode",
            Self::DarkDaltonized => "Dark mode (colorblind-friendly)",
            Self::LightDaltonized => "Light mode (colorblind-friendly)",
            Self::DarkAnsi => "Dark mode (ANSI colors only)",
            Self::LightAnsi => "Light mode (ANSI colors only)",
        }
    }

    pub fn from_setting_value(value: &str) -> Option<Self> {
        let normalized = value.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            "dark-ansi" => Some(Self::DarkAnsi),
            "light-ansi" => Some(Self::LightAnsi),
            "dark-daltonized" => Some(Self::DarkDaltonized),
            "light-daltonized" => Some(Self::LightDaltonized),
            _ => None,
        }
    }

    pub fn from_display_label(value: &str) -> Option<Self> {
        THEME_PICKER_ORDER
            .iter()
            .copied()
            .find(|theme| theme.display_label().eq_ignore_ascii_case(value.trim()))
    }

    pub fn from_config_or_display(value: &str) -> Option<Self> {
        Self::from_setting_value(value).or_else(|| Self::from_display_label(value))
    }
}

/// Maps to: CC `utils/theme.ts:103-109` `THEME_SETTINGS` / `ThemeSetting` — a
/// theme preference as stored in user config. `Auto` follows the terminal's
/// dark/light background and is resolved to a [`ThemeName`] at runtime
/// (`utils/systemTheme.ts`, here `utils/system_theme.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeSetting {
    Auto,
    Named(ThemeName),
}

impl Default for ThemeSetting {
    /// CC's config default and the ThemeContext default (`ThemeProvider.tsx:28`).
    fn default() -> Self {
        Self::Named(ThemeName::Dark)
    }
}

impl ThemeSetting {
    pub fn setting_value(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Named(name) => name.setting_value(),
        }
    }

    /// A stored `theme` value; `None` outside `THEME_SETTINGS`. `auto` is
    /// matched exactly, as CC compares it (`ThemeProvider.tsx:97`); the
    /// theme names keep [`ThemeName::from_setting_value`]'s existing leniency.
    pub fn from_setting_value(value: &str) -> Option<Self> {
        if value == "auto" {
            return Some(Self::Auto);
        }
        ThemeName::from_setting_value(value).map(Self::Named)
    }
}

/// Maps to: CC `Config.tsx:2310-2318` `THEME_LABELS`, which includes `auto`.
pub fn theme_display_label(value: Option<&str>) -> &'static str {
    if value == Some("auto") {
        return "Auto (match terminal)";
    }
    value
        .and_then(ThemeName::from_config_or_display)
        .unwrap_or(ThemeName::Dark)
        .display_label()
}

pub fn get_theme(name: ThemeName) -> &'static Theme {
    match name {
        ThemeName::Dark => &DARK,
        ThemeName::Light => &LIGHT,
        ThemeName::DarkAnsi => &DARK_ANSI,
        ThemeName::LightAnsi => &LIGHT_ANSI,
        ThemeName::DarkDaltonized => &DARK_DALTONIZED,
        ThemeName::LightDaltonized => &LIGHT_DALTONIZED,
    }
}

/// Current active theme — Phase 2: integrate with auto-detect + user config.
pub fn current() -> &'static Theme {
    // Keep the full official color key surface referenced in the main build;
    // this guards future palette edits from silently drifting out of sync.
    let _ = DARK.palette();
    let _ = ThemeColorKey::ALL.map(ThemeColorKey::official_key);
    &DARK
}

/// Map PermissionModeColor to actual theme color. CC's `getModeColor` returns
/// a theme key that the rendering ThemedText resolves against the provided
/// theme, so the caller passes the theme it read from context.
pub fn mode_color(
    t: &Theme,
    c: crate::utils::permissions::permission_mode::PermissionModeColor,
) -> Color {
    use crate::utils::permissions::permission_mode::PermissionModeColor;
    match c {
        PermissionModeColor::Text => t.text,
        PermissionModeColor::Plan => t.plan_mode,
        // CC PermissionMode.ts: acceptEdits → autoAccept (violet ~#af87ff),
        // auto → warning (amber ~#ffc107). Do not merge these colors.
        PermissionModeColor::AutoAccept => t.auto_accept,
        PermissionModeColor::Auto => t.warning,
        PermissionModeColor::Error => t.error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_names_round_trip_official_settings_and_picker_labels() {
        for theme in THEME_PICKER_ORDER {
            assert_eq!(
                ThemeName::from_setting_value(theme.setting_value()),
                Some(theme)
            );
            assert_eq!(
                ThemeName::from_display_label(theme.display_label()),
                Some(theme)
            );
            assert_eq!(
                theme_display_label(Some(theme.setting_value())),
                theme.display_label()
            );
        }
    }

    #[test]
    fn ansi_theme_colors_preserve_official_bright_vs_standard_ansi_names() {
        assert_eq!(DARK_ANSI.ide, Color::DarkBlue);
        assert_eq!(DARK_ANSI.prompt_border, Color::Grey);
        assert_eq!(DARK_ANSI.success, Color::Green);
        assert_eq!(DARK_ANSI.diff_added, Color::DarkGreen);

        assert_eq!(LIGHT_ANSI.permission, Color::DarkBlue);
        assert_eq!(LIGHT_ANSI.permission_shimmer, Color::Blue);
        assert_eq!(LIGHT_ANSI.warning, Color::DarkYellow);
        assert_eq!(LIGHT_ANSI.warning_shimmer, Color::Yellow);
    }

    #[test]
    fn theme_color_keys_cover_the_full_official_palette() {
        assert_eq!(ThemeColorKey::ALL.len(), 69);
        assert_eq!(ThemeColorKey::ClawdBody.official_key(), "clawd_body");
        assert_eq!(
            ThemeColorKey::RateLimitFill.official_key(),
            "rate_limit_fill"
        );

        let palette = DARK.palette();
        assert_eq!(palette[0], DARK.auto_accept);
        assert_eq!(DARK.color(ThemeColorKey::DiffAdded), DARK.diff_added);
        assert_eq!(
            DARK.color(ThemeColorKey::RainbowVioletShimmer),
            DARK.rainbow_violet_shimmer
        );
    }
}
