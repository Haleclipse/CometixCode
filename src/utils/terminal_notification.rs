//! Main-screen-safe seam for official terminal-native notifications.
//!
//! Official Claude Code wires `ToolUseContext.sendOSNotification` to
//! `services/notifier.ts` and `ink/useTerminalNotification.ts`, which may emit
//! terminal OSC/BEL side effects. Nyan keeps the same planning boundary but
//! does not write any raw terminal sequences in the current UI-only phase.

#![allow(dead_code)]

use super::config::GlobalConfig;

pub const DEFAULT_TERMINAL_NOTIFICATION_TITLE: &str = "nyan";
pub const DEFAULT_INTERACTION_THRESHOLD_MS: u64 = 6_000;
pub const IDLE_PROMPT_NOTIFICATION_TYPE: &str = "idle_prompt";
pub const IDLE_PROMPT_NOTIFICATION_MESSAGE: &str = "nyan is waiting for your input";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalNotificationChannel {
    Auto,
    ITerm2,
    ITerm2WithBell,
    Kitty,
    Ghostty,
    TerminalBell,
    Disabled,
    None,
}

impl TerminalNotificationChannel {
    /// Maps to official `preferredNotifChannel` values consumed by
    /// `services/notifier.ts#sendToChannel`.
    pub fn from_config(value: Option<&str>) -> Self {
        let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
            return Self::Auto;
        };
        match value {
            "auto" | "Auto" => Self::Auto,
            "iterm2" | "iTerm2" | "ITerm2" => Self::ITerm2,
            "iterm2_with_bell" | "iTerm2 with bell" | "ITerm2 with bell" => Self::ITerm2WithBell,
            "kitty" | "Kitty" => Self::Kitty,
            "ghostty" | "Ghostty" => Self::Ghostty,
            "terminal_bell" | "Terminal bell" | "Bell" => Self::TerminalBell,
            "notifications_disabled" | "Disabled" | "disabled" => Self::Disabled,
            _ => Self::None,
        }
    }

    pub fn official_config_value(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::ITerm2 => "iterm2",
            Self::ITerm2WithBell => "iterm2_with_bell",
            Self::Kitty => "kitty",
            Self::Ghostty => "ghostty",
            Self::TerminalBell => "terminal_bell",
            Self::Disabled => "notifications_disabled",
            Self::None => "none",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalNotificationMethod {
    ITerm2,
    ITerm2WithBell,
    Kitty,
    Ghostty,
    TerminalBell,
    Disabled,
    NoMethodAvailable,
    None,
}

impl TerminalNotificationMethod {
    /// Official `services/notifier.ts` records this as `method_used`.
    pub fn official_method_used(self) -> &'static str {
        match self {
            Self::ITerm2 => "iterm2",
            Self::ITerm2WithBell => "iterm2_with_bell",
            Self::Kitty => "kitty",
            Self::Ghostty => "ghostty",
            Self::TerminalBell => "terminal_bell",
            Self::Disabled => "disabled",
            Self::NoMethodAvailable => "no_method_available",
            Self::None => "none",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalNotificationEnvironment<'a> {
    pub terminal: Option<&'a str>,
    /// Already-known result of official Apple Terminal bell preference probing.
    /// Nyan does not run `osascript` or `defaults` in the UI-only path, but
    /// callers may provide a safe cached/snapshot value when one exists.
    pub apple_terminal_bell_disabled: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalNotificationEnvironmentSnapshot {
    pub terminal: Option<String>,
    pub apple_terminal_bell_disabled: Option<bool>,
}

impl TerminalNotificationEnvironmentSnapshot {
    pub fn as_environment(&self) -> TerminalNotificationEnvironment<'_> {
        TerminalNotificationEnvironment {
            terminal: self.terminal.as_deref(),
            apple_terminal_bell_disabled: self.apple_terminal_bell_disabled,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalProgressState {
    Running,
    Completed,
    Error,
    Indeterminate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalProgressAction {
    Clear,
    Error { percentage: u8 },
    Indeterminate,
    Set { percentage: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalProgressPlan {
    pub action: TerminalProgressAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessagesTerminalProgressState {
    Disabled,
    Completed,
    Indeterminate,
}

impl MessagesTerminalProgressState {
    fn as_progress_state(self) -> Option<TerminalProgressState> {
        match self {
            Self::Disabled => None,
            Self::Completed => Some(TerminalProgressState::Completed),
            Self::Indeterminate => Some(TerminalProgressState::Indeterminate),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MessagesTerminalProgressMemory {
    pub previous_state: Option<MessagesTerminalProgressState>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalProgressEnvironment<'a> {
    pub stdout_is_tty: bool,
    pub wt_session: bool,
    pub conemu_ansi: bool,
    pub conemu_pid: bool,
    pub conemu_task: bool,
    pub term_program: Option<&'a str>,
    pub term_program_version: Option<&'a str>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalProgressEnvironmentSnapshot {
    pub stdout_is_tty: bool,
    pub wt_session: bool,
    pub conemu_ansi: bool,
    pub conemu_pid: bool,
    pub conemu_task: bool,
    pub term_program: Option<String>,
    pub term_program_version: Option<String>,
}

impl TerminalProgressEnvironmentSnapshot {
    pub fn as_environment(&self) -> TerminalProgressEnvironment<'_> {
        TerminalProgressEnvironment {
            stdout_is_tty: self.stdout_is_tty,
            wt_session: self.wt_session,
            conemu_ansi: self.conemu_ansi,
            conemu_pid: self.conemu_pid,
            conemu_task: self.conemu_task,
            term_program: self.term_program.as_deref(),
            term_program_version: self.term_program_version.as_deref(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalEscapeEnvironment<'a> {
    /// Matches official `env.terminal` for OSC terminator selection. Kitty uses
    /// ST to avoid beeps; other terminals use BEL.
    pub terminal: Option<&'a str>,
    /// Mirrors official `wrapForMultiplexer()` `$TMUX` branch.
    pub tmux: bool,
    /// Mirrors official `wrapForMultiplexer()` `$STY` branch for GNU screen.
    pub screen: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalEscapeEnvironmentSnapshot {
    pub terminal: Option<String>,
    pub tmux: bool,
    pub screen: bool,
}

impl TerminalEscapeEnvironmentSnapshot {
    pub fn as_environment(&self) -> TerminalEscapeEnvironment<'_> {
        TerminalEscapeEnvironment {
            terminal: self.terminal.as_deref(),
            tmux: self.tmux,
            screen: self.screen,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalNotificationOptions {
    pub message: String,
    pub title: Option<String>,
    pub notification_type: String,
}

impl TerminalNotificationOptions {
    pub fn new(notification_type: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            title: None,
            notification_type: notification_type.into(),
        }
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn title_or_default(&self) -> String {
        self.title
            .clone()
            .unwrap_or_else(|| DEFAULT_TERMINAL_NOTIFICATION_TITLE.to_string())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalNotificationRequest {
    pub options: TerminalNotificationOptions,
    pub channel: TerminalNotificationChannel,
    pub method: TerminalNotificationMethod,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalNotificationHookInput {
    pub hook_event_name: &'static str,
    pub message: String,
    pub title: Option<String>,
    pub notification_type: String,
}

impl TerminalNotificationHookInput {
    pub fn from_options(options: &TerminalNotificationOptions) -> Self {
        Self {
            hook_event_name: "Notification",
            message: options.message.clone(),
            title: options.title.clone(),
            notification_type: options.notification_type.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalNotificationServicePlan {
    pub hook_input: TerminalNotificationHookInput,
    pub request: TerminalNotificationRequest,
    pub configured_channel: &'static str,
    pub method_used: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotifyAfterTimeoutSnapshot<'a> {
    pub message: &'a str,
    pub notification_type: &'a str,
    pub now_ms: u64,
    pub last_interaction_ms: u64,
    pub threshold_ms: u64,
    pub node_env: Option<&'a str>,
    pub has_notified: bool,
    pub channel: TerminalNotificationChannel,
    pub environment: TerminalNotificationEnvironment<'a>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplIdlePromptNotificationSnapshot<'a> {
    pub last_query_completion_ms: Option<u64>,
    pub last_interaction_ms: u64,
    pub now_ms: u64,
    pub message_idle_notif_threshold_ms: u64,
    pub is_loading: bool,
    /// Maps to official `toolJSX` presence while keeping Rust naming at the
    /// main-screen local command UI boundary.
    pub has_active_local_command_ui: bool,
    pub has_focused_input_dialog: bool,
    pub channel: TerminalNotificationChannel,
    pub environment: TerminalNotificationEnvironment<'a>,
}

impl TerminalNotificationRequest {
    pub fn plan(
        options: TerminalNotificationOptions,
        channel: TerminalNotificationChannel,
        terminal: Option<&str>,
    ) -> Self {
        let method = resolve_terminal_notification_method(channel, terminal);
        Self {
            options,
            channel,
            method,
        }
    }

    pub fn plan_with_environment(
        options: TerminalNotificationOptions,
        channel: TerminalNotificationChannel,
        env: &TerminalNotificationEnvironment<'_>,
    ) -> Self {
        let method = resolve_terminal_notification_method_with_environment(channel, env);
        Self {
            options,
            channel,
            method,
        }
    }
}

pub fn terminal_notification_request_from_config(
    options: TerminalNotificationOptions,
    config: &GlobalConfig,
    env: &TerminalNotificationEnvironment<'_>,
) -> TerminalNotificationRequest {
    let channel =
        TerminalNotificationChannel::from_config(config.preferred_notif_channel.as_deref());
    TerminalNotificationRequest::plan_with_environment(options, channel, env)
}

/// Pure counterpart of official `services/notifier.ts#sendNotification`.
/// It builds the notification-hook input and terminal request from readonly
/// config/environment snapshots, but never executes hooks, logs analytics, or
/// emits terminal/native notification bytes.
pub fn terminal_notification_service_plan_from_config(
    options: TerminalNotificationOptions,
    config: &GlobalConfig,
    env: &TerminalNotificationEnvironment<'_>,
) -> TerminalNotificationServicePlan {
    let hook_input = TerminalNotificationHookInput::from_options(&options);
    let request = terminal_notification_request_from_config(options, config, env);
    TerminalNotificationServicePlan {
        hook_input,
        configured_channel: request.channel.official_config_value(),
        method_used: request.method.official_method_used(),
        request,
    }
}

pub fn terminal_notification_environment_snapshot(
    terminal: Option<String>,
    apple_terminal_bell_disabled: Option<bool>,
) -> TerminalNotificationEnvironmentSnapshot {
    TerminalNotificationEnvironmentSnapshot {
        terminal,
        apple_terminal_bell_disabled,
    }
}

pub fn terminal_progress_environment_snapshot_from_env(
    stdout_is_tty: bool,
    get_env: impl Fn(&str) -> Option<String>,
) -> TerminalProgressEnvironmentSnapshot {
    TerminalProgressEnvironmentSnapshot {
        stdout_is_tty,
        wt_session: get_env("WT_SESSION").is_some(),
        conemu_ansi: get_env("ConEmuANSI").is_some(),
        conemu_pid: get_env("ConEmuPID").is_some(),
        conemu_task: get_env("ConEmuTask").is_some(),
        term_program: get_env("TERM_PROGRAM"),
        term_program_version: get_env("TERM_PROGRAM_VERSION"),
    }
}

pub fn terminal_escape_environment_snapshot_from_env(
    terminal: Option<String>,
    get_env: impl Fn(&str) -> Option<String>,
) -> TerminalEscapeEnvironmentSnapshot {
    TerminalEscapeEnvironmentSnapshot {
        terminal,
        tmux: get_env("TMUX").is_some(),
        screen: get_env("STY").is_some(),
    }
}

/// Pure planner for official terminal notification routing. It intentionally
/// returns the method that would be used instead of emitting OSC/BEL bytes.
pub fn resolve_terminal_notification_method(
    channel: TerminalNotificationChannel,
    terminal: Option<&str>,
) -> TerminalNotificationMethod {
    resolve_terminal_notification_method_with_environment(
        channel,
        &TerminalNotificationEnvironment {
            terminal,
            apple_terminal_bell_disabled: None,
        },
    )
}

pub fn resolve_terminal_notification_method_with_environment(
    channel: TerminalNotificationChannel,
    env: &TerminalNotificationEnvironment<'_>,
) -> TerminalNotificationMethod {
    match channel {
        TerminalNotificationChannel::Auto => resolve_auto_terminal_notification_method(env),
        TerminalNotificationChannel::ITerm2 => TerminalNotificationMethod::ITerm2,
        TerminalNotificationChannel::ITerm2WithBell => TerminalNotificationMethod::ITerm2WithBell,
        TerminalNotificationChannel::Kitty => TerminalNotificationMethod::Kitty,
        TerminalNotificationChannel::Ghostty => TerminalNotificationMethod::Ghostty,
        TerminalNotificationChannel::TerminalBell => TerminalNotificationMethod::TerminalBell,
        TerminalNotificationChannel::Disabled => TerminalNotificationMethod::Disabled,
        TerminalNotificationChannel::None => TerminalNotificationMethod::None,
    }
}

pub fn should_notify_after_timeout(
    node_env: Option<&str>,
    now_ms: u64,
    last_interaction_ms: u64,
    threshold_ms: u64,
    has_notified: bool,
) -> bool {
    node_env != Some("test")
        && !has_notified
        && now_ms.saturating_sub(last_interaction_ms) >= threshold_ms
}

/// Pure counterpart of official `hooks/useNotifyAfterTimeout.ts`.
/// It plans the OS-level notification request after the interaction threshold
/// expires, but never starts timers or emits terminal/native notification bytes.
pub fn notify_after_timeout_request(
    snapshot: &NotifyAfterTimeoutSnapshot<'_>,
) -> Option<TerminalNotificationRequest> {
    if !should_notify_after_timeout(
        snapshot.node_env,
        snapshot.now_ms,
        snapshot.last_interaction_ms,
        snapshot.threshold_ms,
        snapshot.has_notified,
    ) {
        return None;
    }

    Some(TerminalNotificationRequest::plan_with_environment(
        TerminalNotificationOptions::new(snapshot.notification_type, snapshot.message),
        snapshot.channel,
        &snapshot.environment,
    ))
}

/// Pure counterpart of the REPL idle-input OS notification effect. Official
/// sends `idle_prompt` after a completed response if the user has not interacted
/// and no loading/local-command/dialog UI is active. Nyan only returns the
/// planned request for an explicit future side-effect layer.
pub fn repl_idle_prompt_notification_request(
    snapshot: &ReplIdlePromptNotificationSnapshot<'_>,
) -> Option<TerminalNotificationRequest> {
    let completion_ms = snapshot
        .last_query_completion_ms
        .filter(|value| *value > 0)?;

    if snapshot.is_loading
        || snapshot.has_active_local_command_ui
        || snapshot.has_focused_input_dialog
        || snapshot.last_interaction_ms > completion_ms
        || snapshot.now_ms.saturating_sub(completion_ms) < snapshot.message_idle_notif_threshold_ms
    {
        return None;
    }

    Some(TerminalNotificationRequest::plan_with_environment(
        TerminalNotificationOptions::new(
            IDLE_PROMPT_NOTIFICATION_TYPE,
            IDLE_PROMPT_NOTIFICATION_MESSAGE,
        ),
        snapshot.channel,
        &snapshot.environment,
    ))
}

fn resolve_auto_terminal_notification_method(
    env: &TerminalNotificationEnvironment<'_>,
) -> TerminalNotificationMethod {
    match env.terminal {
        Some("iTerm.app") => TerminalNotificationMethod::ITerm2,
        Some("kitty") => TerminalNotificationMethod::Kitty,
        Some("ghostty") => TerminalNotificationMethod::Ghostty,
        Some("Apple_Terminal") if env.apple_terminal_bell_disabled == Some(true) => {
            TerminalNotificationMethod::TerminalBell
        }
        // Official probes Apple Terminal preferences with `osascript`/`defaults`.
        // The UI-only seam must not execute external commands, so unknown or
        // false probe results expose the safe no-method branch.
        Some("Apple_Terminal") => TerminalNotificationMethod::NoMethodAvailable,
        _ => TerminalNotificationMethod::NoMethodAvailable,
    }
}

/// Pure counterpart of official `terminal.ts#isProgressReportingAvailable()`.
/// It accepts an explicit environment snapshot so tests and UI code can reason
/// about support without reading global process state or writing OSC bytes.
pub fn is_progress_reporting_available(env: &TerminalProgressEnvironment<'_>) -> bool {
    if !env.stdout_is_tty || env.wt_session {
        return false;
    }

    if env.conemu_ansi || env.conemu_pid || env.conemu_task {
        return true;
    }

    let Some(version) = env.term_program_version.and_then(coerce_semver_triplet) else {
        return false;
    };

    match env.term_program {
        Some("ghostty") => version >= (1, 2, 0),
        Some("iTerm.app") => version >= (3, 6, 6),
        _ => false,
    }
}

/// Pure counterpart of the progress-producing effect in official
/// `components/Messages.tsx`. The caller supplies already-known app state;
/// this function only decides which progress state should be requested.
pub fn messages_terminal_progress_state(
    terminal_progress_bar_enabled: bool,
    proactive_active: bool,
    has_tools_in_progress: bool,
) -> MessagesTerminalProgressState {
    if !terminal_progress_bar_enabled || proactive_active {
        return MessagesTerminalProgressState::Disabled;
    }

    if has_tools_in_progress {
        MessagesTerminalProgressState::Indeterminate
    } else {
        MessagesTerminalProgressState::Completed
    }
}

/// Plans the official `Messages.tsx` terminal progress side effect while
/// preserving the official duplicate suppression shape. No terminal bytes are
/// written here; unavailable terminals simply return no plan after updating the
/// remembered state, matching the official effect's pre-write state update.
pub fn messages_terminal_progress_plan(
    memory: &mut MessagesTerminalProgressMemory,
    progress_reporting_available: bool,
    terminal_progress_bar_enabled: bool,
    proactive_active: bool,
    has_tools_in_progress: bool,
) -> Option<TerminalProgressPlan> {
    let next_state = messages_terminal_progress_state(
        terminal_progress_bar_enabled,
        proactive_active,
        has_tools_in_progress,
    );

    if memory.previous_state == Some(next_state)
        || (memory.previous_state.is_none()
            && next_state == MessagesTerminalProgressState::Disabled)
    {
        memory.previous_state = Some(next_state);
        return None;
    }

    memory.previous_state = Some(next_state);
    terminal_progress_plan(
        progress_reporting_available,
        next_state.as_progress_state(),
        None,
    )
}

/// Official `Messages.tsx` clears terminal progress during unmount cleanup.
pub fn messages_terminal_progress_cleanup_plan(
    progress_reporting_available: bool,
) -> Option<TerminalProgressPlan> {
    terminal_progress_plan(progress_reporting_available, None, None)
}

fn coerce_semver_triplet(value: &str) -> Option<(u64, u64, u64)> {
    let start = value.find(|ch: char| ch.is_ascii_digit())?;
    let mut rest = &value[start..];
    let mut parts = [0_u64; 3];

    for index in 0..parts.len() {
        let digit_len = rest
            .char_indices()
            .find_map(|(idx, ch)| (!ch.is_ascii_digit()).then_some(idx))
            .unwrap_or(rest.len());
        if digit_len == 0 {
            return (index > 0).then_some((parts[0], parts[1], parts[2]));
        }
        parts[index] = rest[..digit_len].parse().ok()?;
        rest = &rest[digit_len..];
        if !rest.starts_with('.') {
            break;
        }
        rest = &rest[1..];
    }

    Some((parts[0], parts[1], parts[2]))
}

/// Pure counterpart of official `useTerminalNotification().progress(...)`.
/// It plans the OSC 9;4 action that would be emitted when terminal progress
/// reporting is available, but never writes raw OSC/BEL bytes in Nyan.
pub fn terminal_progress_plan(
    progress_reporting_available: bool,
    state: Option<TerminalProgressState>,
    percentage: Option<f64>,
) -> Option<TerminalProgressPlan> {
    if !progress_reporting_available {
        return None;
    }

    let action = match state {
        None | Some(TerminalProgressState::Completed) => TerminalProgressAction::Clear,
        Some(TerminalProgressState::Error) => TerminalProgressAction::Error {
            percentage: clamp_progress_percentage(percentage),
        },
        Some(TerminalProgressState::Indeterminate) => TerminalProgressAction::Indeterminate,
        Some(TerminalProgressState::Running) => TerminalProgressAction::Set {
            percentage: clamp_progress_percentage(percentage),
        },
    };

    Some(TerminalProgressPlan { action })
}

fn clamp_progress_percentage(percentage: Option<f64>) -> u8 {
    percentage.unwrap_or(0.0).round().clamp(0.0, 100.0) as u8
}

fn notification_osc_sequence(env: &TerminalEscapeEnvironment<'_>, parts: &[String]) -> String {
    let raw = format!("\x1b]{}{}", parts.join(";"), osc_terminator(env));
    wrap_for_multiplexer(&raw, env)
}

fn terminal_bell_sequence() -> String {
    "\x07".to_string()
}

fn iterm2_notification_sequence(
    options: &TerminalNotificationOptions,
    env: &TerminalEscapeEnvironment<'_>,
) -> String {
    let display = if let Some(title) = options.title.as_deref() {
        format!("{title}:\n{}", options.message)
    } else {
        options.message.clone()
    };
    notification_osc_sequence(env, &["9".to_string(), format!("\n\n{display}")])
}

fn kitty_notification_sequences(
    options: &TerminalNotificationOptions,
    env: &TerminalEscapeEnvironment<'_>,
    kitty_id: u32,
) -> Vec<String> {
    let title = options.title_or_default();
    vec![
        notification_osc_sequence(
            env,
            &["99".to_string(), format!("i={kitty_id}:d=0:p=title"), title],
        ),
        notification_osc_sequence(
            env,
            &[
                "99".to_string(),
                format!("i={kitty_id}:p=body"),
                options.message.clone(),
            ],
        ),
        notification_osc_sequence(
            env,
            &[
                "99".to_string(),
                format!("i={kitty_id}:d=1:a=focus"),
                String::new(),
            ],
        ),
    ]
}

fn ghostty_notification_sequence(
    options: &TerminalNotificationOptions,
    env: &TerminalEscapeEnvironment<'_>,
) -> String {
    notification_osc_sequence(
        env,
        &[
            "777".to_string(),
            "notify".to_string(),
            options.title_or_default(),
            options.message.clone(),
        ],
    )
}

/// Pure encoder for official `useTerminalNotification()` notification output.
/// The caller supplies the kitty id because official `generateKittyId()` is
/// random. Nyan returns encoded sequences for an explicitly enabled future
/// side-effect layer, but the default UI-only path does not write them.
pub fn terminal_notification_escape_sequences(
    request: &TerminalNotificationRequest,
    env: &TerminalEscapeEnvironment<'_>,
    kitty_id: u32,
) -> Vec<String> {
    match request.method {
        TerminalNotificationMethod::ITerm2 => {
            vec![iterm2_notification_sequence(&request.options, env)]
        }
        TerminalNotificationMethod::ITerm2WithBell => vec![
            iterm2_notification_sequence(&request.options, env),
            terminal_bell_sequence(),
        ],
        TerminalNotificationMethod::Kitty => {
            kitty_notification_sequences(&request.options, env, kitty_id)
        }
        TerminalNotificationMethod::Ghostty => {
            vec![ghostty_notification_sequence(&request.options, env)]
        }
        TerminalNotificationMethod::TerminalBell => vec![terminal_bell_sequence()],
        TerminalNotificationMethod::Disabled
        | TerminalNotificationMethod::NoMethodAvailable
        | TerminalNotificationMethod::None => Vec::new(),
    }
}

fn progress_action_parts(action: TerminalProgressAction) -> (&'static str, String) {
    match action {
        TerminalProgressAction::Clear => ("0", String::new()),
        TerminalProgressAction::Set { percentage } => ("1", percentage.to_string()),
        TerminalProgressAction::Error { percentage } => ("2", percentage.to_string()),
        TerminalProgressAction::Indeterminate => ("3", String::new()),
    }
}

fn osc_terminator(env: &TerminalEscapeEnvironment<'_>) -> &'static str {
    if env.terminal == Some("kitty") {
        "\x1b\\"
    } else {
        "\x07"
    }
}

/// Pure encoder for official `useTerminalNotification().progress(...)` OSC 9;4
/// output. This deliberately returns the bytes that an explicitly enabled
/// side-effect layer would write; Nyan's default main-screen path only plans
/// or captures the sequence and does not emit raw OSC/BEL bytes.
pub fn terminal_progress_escape_sequence(
    plan: TerminalProgressPlan,
    env: &TerminalEscapeEnvironment<'_>,
) -> String {
    let (operation, value) = progress_action_parts(plan.action);
    let raw = format!("\x1b]9;4;{operation};{value}{}", osc_terminator(env));
    wrap_for_multiplexer(&raw, env)
}

fn wrap_for_multiplexer(sequence: &str, env: &TerminalEscapeEnvironment<'_>) -> String {
    if env.tmux {
        let escaped = sequence.replace('\x1b', "\x1b\x1b");
        return format!("\x1bPtmux;{escaped}\x1b\\");
    }
    if env.screen {
        return format!("\x1bP{sequence}\x1b\\");
    }
    sequence.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_notification_channel_parses_official_config_values() {
        assert_eq!(
            TerminalNotificationChannel::from_config(None),
            TerminalNotificationChannel::Auto
        );
        assert_eq!(
            TerminalNotificationChannel::from_config(Some("iterm2_with_bell")),
            TerminalNotificationChannel::ITerm2WithBell
        );
        assert_eq!(
            TerminalNotificationChannel::from_config(Some("notifications_disabled")),
            TerminalNotificationChannel::Disabled
        );
        assert_eq!(
            TerminalNotificationChannel::from_config(Some("unknown")),
            TerminalNotificationChannel::None
        );
    }

    #[test]
    fn terminal_notification_auto_maps_supported_terminals_without_writes() {
        assert_eq!(
            resolve_terminal_notification_method(TerminalNotificationChannel::Auto, Some("kitty")),
            TerminalNotificationMethod::Kitty
        );
        assert_eq!(
            resolve_terminal_notification_method(
                TerminalNotificationChannel::Auto,
                Some("iTerm.app")
            ),
            TerminalNotificationMethod::ITerm2
        );
        assert_eq!(
            resolve_terminal_notification_method(
                TerminalNotificationChannel::Auto,
                Some("ghostty")
            ),
            TerminalNotificationMethod::Ghostty
        );
        assert_eq!(
            resolve_terminal_notification_method(
                TerminalNotificationChannel::Auto,
                Some("Apple_Terminal")
            ),
            TerminalNotificationMethod::NoMethodAvailable
        );
    }

    #[test]
    fn terminal_notification_auto_uses_readonly_apple_terminal_bell_snapshot_when_available() {
        let env = TerminalNotificationEnvironment {
            terminal: Some("Apple_Terminal"),
            apple_terminal_bell_disabled: Some(true),
        };
        let request = TerminalNotificationRequest::plan_with_environment(
            TerminalNotificationOptions::new("task_complete", "Done"),
            TerminalNotificationChannel::Auto,
            &env,
        );

        assert_eq!(request.method, TerminalNotificationMethod::TerminalBell);
        assert_eq!(
            terminal_notification_escape_sequences(
                &request,
                &TerminalEscapeEnvironment::default(),
                7,
            ),
            vec!["\x07".to_string()]
        );

        let unknown_preference = TerminalNotificationEnvironment {
            terminal: Some("Apple_Terminal"),
            apple_terminal_bell_disabled: None,
        };
        assert_eq!(
            resolve_terminal_notification_method_with_environment(
                TerminalNotificationChannel::Auto,
                &unknown_preference,
            ),
            TerminalNotificationMethod::NoMethodAvailable
        );
    }

    #[test]
    fn terminal_notification_request_keeps_default_title_and_method_plan() {
        let request = TerminalNotificationRequest::plan(
            TerminalNotificationOptions::new("task_complete", "Done"),
            TerminalNotificationChannel::Kitty,
            Some("kitty"),
        );

        assert_eq!(request.method, TerminalNotificationMethod::Kitty);
        assert_eq!(
            request.options.title_or_default(),
            DEFAULT_TERMINAL_NOTIFICATION_TITLE
        );
    }

    #[test]
    fn terminal_notification_environment_snapshot_preserves_apple_terminal_bell_cache() {
        let snapshot = terminal_notification_environment_snapshot(
            Some("Apple_Terminal".to_string()),
            Some(true),
        );

        assert_eq!(
            resolve_terminal_notification_method_with_environment(
                TerminalNotificationChannel::Auto,
                &snapshot.as_environment(),
            ),
            TerminalNotificationMethod::TerminalBell
        );
    }

    #[test]
    fn terminal_progress_environment_snapshot_reads_official_env_without_side_effects() {
        let snapshot = terminal_progress_environment_snapshot_from_env(true, |key| match key {
            "TERM_PROGRAM" => Some("iTerm.app".to_string()),
            "TERM_PROGRAM_VERSION" => Some("Build 3.6.6".to_string()),
            _ => None,
        });

        assert!(is_progress_reporting_available(&snapshot.as_environment()));

        let windows_terminal =
            terminal_progress_environment_snapshot_from_env(true, |key| match key {
                "WT_SESSION" => Some("session".to_string()),
                "TERM_PROGRAM" => Some("iTerm.app".to_string()),
                "TERM_PROGRAM_VERSION" => Some("3.6.6".to_string()),
                _ => None,
            });
        assert!(!is_progress_reporting_available(
            &windows_terminal.as_environment()
        ));

        let conemu = terminal_progress_environment_snapshot_from_env(true, |key| match key {
            "ConEmuTask" => Some("{Shells::cmd}".to_string()),
            _ => None,
        });
        assert!(is_progress_reporting_available(&conemu.as_environment()));
    }

    #[test]
    fn terminal_escape_environment_snapshot_reads_multiplexer_env_without_emitting() {
        let snapshot =
            terminal_escape_environment_snapshot_from_env(Some("kitty".to_string()), |key| {
                (key == "TMUX").then(|| "/tmp/tmux".to_string())
            });

        assert_eq!(
            terminal_progress_escape_sequence(
                TerminalProgressPlan {
                    action: TerminalProgressAction::Indeterminate,
                },
                &snapshot.as_environment(),
            ),
            "\x1bPtmux;\x1b\x1b]9;4;3;\x1b\x1b\\\x1b\\"
        );

        let screen = terminal_escape_environment_snapshot_from_env(None, |key| {
            (key == "STY").then(|| "screen".to_string())
        });
        assert_eq!(
            terminal_progress_escape_sequence(
                TerminalProgressPlan {
                    action: TerminalProgressAction::Clear,
                },
                &screen.as_environment(),
            ),
            "\x1bP\x1b]9;4;0;\x07\x1b\\"
        );
    }

    #[test]
    fn terminal_notification_service_plan_uses_readonly_config_channel_and_hook_input() {
        let mut config = GlobalConfig::default();
        config.preferred_notif_channel = Some("ghostty".to_string());
        let env = TerminalNotificationEnvironment {
            terminal: Some("kitty"),
            apple_terminal_bell_disabled: None,
        };

        let plan = terminal_notification_service_plan_from_config(
            TerminalNotificationOptions::new("idle_prompt", "Nyan is waiting").with_title("Custom"),
            &config,
            &env,
        );

        assert_eq!(plan.configured_channel, "ghostty");
        assert_eq!(plan.method_used, "ghostty");
        assert_eq!(plan.request.channel, TerminalNotificationChannel::Ghostty);
        assert_eq!(plan.request.method, TerminalNotificationMethod::Ghostty);
        assert_eq!(plan.hook_input.hook_event_name, "Notification");
        assert_eq!(plan.hook_input.message, "Nyan is waiting");
        assert_eq!(plan.hook_input.title.as_deref(), Some("Custom"));
        assert_eq!(plan.hook_input.notification_type, "idle_prompt");
    }

    #[test]
    fn terminal_notification_service_plan_defaults_to_auto_channel_from_config() {
        let config = GlobalConfig::default();
        let env = TerminalNotificationEnvironment {
            terminal: Some("kitty"),
            apple_terminal_bell_disabled: None,
        };

        let plan = terminal_notification_service_plan_from_config(
            TerminalNotificationOptions::new("task_complete", "Done"),
            &config,
            &env,
        );

        assert_eq!(plan.configured_channel, "auto");
        assert_eq!(plan.method_used, "kitty");
        assert_eq!(plan.request.channel, TerminalNotificationChannel::Auto);
        assert_eq!(plan.request.method, TerminalNotificationMethod::Kitty);
    }

    #[test]
    fn notify_after_timeout_plans_official_idle_threshold_without_emitting() {
        let mut snapshot = NotifyAfterTimeoutSnapshot {
            message: "Build finished",
            notification_type: "task_complete",
            now_ms: 12_000,
            last_interaction_ms: 5_999,
            threshold_ms: DEFAULT_INTERACTION_THRESHOLD_MS,
            node_env: None,
            has_notified: false,
            channel: TerminalNotificationChannel::Kitty,
            environment: TerminalNotificationEnvironment {
                terminal: Some("kitty"),
                apple_terminal_bell_disabled: None,
            },
        };

        let request = notify_after_timeout_request(&snapshot)
            .expect("elapsed interaction threshold should plan a request");
        assert_eq!(request.method, TerminalNotificationMethod::Kitty);
        assert_eq!(request.options.message, "Build finished");
        assert_eq!(request.options.notification_type, "task_complete");

        snapshot.last_interaction_ms = 6_001;
        assert!(notify_after_timeout_request(&snapshot).is_none());

        snapshot.last_interaction_ms = 0;
        snapshot.node_env = Some("test");
        assert!(notify_after_timeout_request(&snapshot).is_none());

        snapshot.node_env = None;
        snapshot.has_notified = true;
        assert!(notify_after_timeout_request(&snapshot).is_none());
    }

    #[test]
    fn repl_idle_prompt_notification_matches_official_repl_gate() {
        let base = ReplIdlePromptNotificationSnapshot {
            last_query_completion_ms: Some(1_000),
            last_interaction_ms: 1_000,
            now_ms: 61_000,
            message_idle_notif_threshold_ms: 60_000,
            is_loading: false,
            has_active_local_command_ui: false,
            has_focused_input_dialog: false,
            channel: TerminalNotificationChannel::Auto,
            environment: TerminalNotificationEnvironment {
                terminal: Some("ghostty"),
                apple_terminal_bell_disabled: None,
            },
        };

        let request = repl_idle_prompt_notification_request(&base)
            .expect("idle prompt should plan after the configured threshold");
        assert_eq!(request.method, TerminalNotificationMethod::Ghostty);
        assert_eq!(
            request.options.notification_type,
            IDLE_PROMPT_NOTIFICATION_TYPE
        );
        assert_eq!(request.options.message, IDLE_PROMPT_NOTIFICATION_MESSAGE);

        let recent_user_interaction = ReplIdlePromptNotificationSnapshot {
            last_interaction_ms: 1_001,
            ..base
        };
        assert!(repl_idle_prompt_notification_request(&recent_user_interaction).is_none());

        let still_loading = ReplIdlePromptNotificationSnapshot {
            is_loading: true,
            ..base
        };
        assert!(repl_idle_prompt_notification_request(&still_loading).is_none());

        let local_command_open = ReplIdlePromptNotificationSnapshot {
            has_active_local_command_ui: true,
            ..base
        };
        assert!(repl_idle_prompt_notification_request(&local_command_open).is_none());

        let below_threshold = ReplIdlePromptNotificationSnapshot {
            now_ms: 60_999,
            ..base
        };
        assert!(repl_idle_prompt_notification_request(&below_threshold).is_none());
    }

    #[test]
    fn progress_reporting_availability_matches_official_terminal_gate() {
        assert!(!is_progress_reporting_available(
            &TerminalProgressEnvironment {
                stdout_is_tty: false,
                term_program: Some("iTerm.app"),
                term_program_version: Some("3.6.6"),
                ..Default::default()
            }
        ));
        assert!(!is_progress_reporting_available(
            &TerminalProgressEnvironment {
                stdout_is_tty: true,
                wt_session: true,
                conemu_ansi: true,
                ..Default::default()
            }
        ));
        assert!(is_progress_reporting_available(
            &TerminalProgressEnvironment {
                stdout_is_tty: true,
                conemu_task: true,
                ..Default::default()
            }
        ));
        assert!(is_progress_reporting_available(
            &TerminalProgressEnvironment {
                stdout_is_tty: true,
                term_program: Some("ghostty"),
                term_program_version: Some("1.2.0"),
                ..Default::default()
            }
        ));
        assert!(!is_progress_reporting_available(
            &TerminalProgressEnvironment {
                stdout_is_tty: true,
                term_program: Some("ghostty"),
                term_program_version: Some("1.1.9"),
                ..Default::default()
            }
        ));
        assert!(is_progress_reporting_available(
            &TerminalProgressEnvironment {
                stdout_is_tty: true,
                term_program: Some("iTerm.app"),
                term_program_version: Some("Build 3.6.6"),
                ..Default::default()
            }
        ));
        assert!(!is_progress_reporting_available(
            &TerminalProgressEnvironment {
                stdout_is_tty: true,
                term_program: Some("iTerm.app"),
                term_program_version: Some("3.6.5"),
                ..Default::default()
            }
        ));
    }

    #[test]
    fn progress_reporting_availability_rejects_unknown_or_unversioned_terminals() {
        assert!(!is_progress_reporting_available(
            &TerminalProgressEnvironment {
                stdout_is_tty: true,
                term_program: Some("kitty"),
                term_program_version: Some("0.40.0"),
                ..Default::default()
            }
        ));
        assert!(!is_progress_reporting_available(
            &TerminalProgressEnvironment {
                stdout_is_tty: true,
                term_program: Some("iTerm.app"),
                term_program_version: Some("not-a-version"),
                ..Default::default()
            }
        ));
    }

    #[test]
    fn messages_terminal_progress_state_matches_official_messages_gate() {
        assert_eq!(
            messages_terminal_progress_state(true, false, false),
            MessagesTerminalProgressState::Completed
        );
        assert_eq!(
            messages_terminal_progress_state(true, false, true),
            MessagesTerminalProgressState::Indeterminate
        );
        assert_eq!(
            messages_terminal_progress_state(false, false, true),
            MessagesTerminalProgressState::Disabled
        );
        assert_eq!(
            messages_terminal_progress_state(true, true, true),
            MessagesTerminalProgressState::Disabled
        );
    }

    #[test]
    fn messages_terminal_progress_plan_dedupes_like_official_effect() {
        let mut memory = MessagesTerminalProgressMemory::default();

        assert_eq!(
            messages_terminal_progress_plan(&mut memory, true, false, false, false),
            None
        );
        assert_eq!(
            memory.previous_state,
            Some(MessagesTerminalProgressState::Disabled)
        );

        assert_eq!(
            messages_terminal_progress_plan(&mut memory, true, true, false, false)
                .map(|plan| plan.action),
            Some(TerminalProgressAction::Clear)
        );
        assert_eq!(
            messages_terminal_progress_plan(&mut memory, true, true, false, false),
            None
        );

        assert_eq!(
            messages_terminal_progress_plan(&mut memory, true, true, false, true)
                .map(|plan| plan.action),
            Some(TerminalProgressAction::Indeterminate)
        );
        assert_eq!(
            messages_terminal_progress_plan(&mut memory, true, true, false, true),
            None
        );

        assert_eq!(
            messages_terminal_progress_plan(&mut memory, true, false, false, true)
                .map(|plan| plan.action),
            Some(TerminalProgressAction::Clear)
        );
    }

    #[test]
    fn messages_terminal_progress_plan_tracks_state_even_when_terminal_is_unavailable() {
        let mut memory = MessagesTerminalProgressMemory::default();

        assert_eq!(
            messages_terminal_progress_plan(&mut memory, false, true, false, true),
            None
        );
        assert_eq!(
            memory.previous_state,
            Some(MessagesTerminalProgressState::Indeterminate)
        );

        assert_eq!(
            messages_terminal_progress_plan(&mut memory, true, true, false, true),
            None
        );
        assert_eq!(
            messages_terminal_progress_plan(&mut memory, true, true, false, false)
                .map(|plan| plan.action),
            Some(TerminalProgressAction::Clear)
        );
    }

    #[test]
    fn messages_terminal_progress_cleanup_clears_when_supported() {
        assert_eq!(
            messages_terminal_progress_cleanup_plan(true).map(|plan| plan.action),
            Some(TerminalProgressAction::Clear)
        );
        assert_eq!(messages_terminal_progress_cleanup_plan(false), None);
    }

    #[test]
    fn terminal_progress_plan_matches_official_progress_actions_without_writes() {
        assert_eq!(
            terminal_progress_plan(true, None, Some(42.0)).map(|plan| plan.action),
            Some(TerminalProgressAction::Clear)
        );
        assert_eq!(
            terminal_progress_plan(true, Some(TerminalProgressState::Completed), Some(42.0))
                .map(|plan| plan.action),
            Some(TerminalProgressAction::Clear)
        );
        assert_eq!(
            terminal_progress_plan(true, Some(TerminalProgressState::Indeterminate), None)
                .map(|plan| plan.action),
            Some(TerminalProgressAction::Indeterminate)
        );
        assert_eq!(
            terminal_progress_plan(true, Some(TerminalProgressState::Running), Some(12.4))
                .map(|plan| plan.action),
            Some(TerminalProgressAction::Set { percentage: 12 })
        );
        assert_eq!(
            terminal_progress_plan(true, Some(TerminalProgressState::Error), Some(12.5))
                .map(|plan| plan.action),
            Some(TerminalProgressAction::Error { percentage: 13 })
        );
    }

    #[test]
    fn terminal_progress_plan_clamps_percentage_and_skips_unavailable_terminals() {
        assert_eq!(
            terminal_progress_plan(true, Some(TerminalProgressState::Running), Some(-5.0))
                .map(|plan| plan.action),
            Some(TerminalProgressAction::Set { percentage: 0 })
        );
        assert_eq!(
            terminal_progress_plan(true, Some(TerminalProgressState::Running), Some(150.0))
                .map(|plan| plan.action),
            Some(TerminalProgressAction::Set { percentage: 100 })
        );
        assert_eq!(
            terminal_progress_plan(true, Some(TerminalProgressState::Running), None)
                .map(|plan| plan.action),
            Some(TerminalProgressAction::Set { percentage: 0 })
        );
        assert!(
            terminal_progress_plan(true, Some(TerminalProgressState::Running), Some(f64::NAN))
                .is_some()
        );
        assert_eq!(
            terminal_progress_plan(false, Some(TerminalProgressState::Running), Some(50.0)),
            None
        );
    }

    #[test]
    fn terminal_notification_escape_sequences_match_official_notification_bytes_without_emitting() {
        let env = TerminalEscapeEnvironment::default();
        let iterm2 = TerminalNotificationRequest::plan(
            TerminalNotificationOptions::new("task_complete", "Done").with_title("Build"),
            TerminalNotificationChannel::ITerm2,
            Some("iTerm.app"),
        );
        assert_eq!(
            terminal_notification_escape_sequences(&iterm2, &env, 7),
            vec!["\x1b]9;\n\nBuild:\nDone\x07".to_string()]
        );

        let ghostty = TerminalNotificationRequest::plan(
            TerminalNotificationOptions::new("task_complete", "Done").with_title("Build"),
            TerminalNotificationChannel::Ghostty,
            Some("ghostty"),
        );
        assert_eq!(
            terminal_notification_escape_sequences(&ghostty, &env, 7),
            vec!["\x1b]777;notify;Build;Done\x07".to_string()]
        );

        let disabled = TerminalNotificationRequest::plan(
            TerminalNotificationOptions::new("task_complete", "Done"),
            TerminalNotificationChannel::Disabled,
            Some("kitty"),
        );
        assert!(terminal_notification_escape_sequences(&disabled, &env, 7).is_empty());
    }

    #[test]
    fn terminal_notification_escape_sequences_match_official_kitty_and_bell_shape() {
        let kitty = TerminalNotificationRequest::plan(
            TerminalNotificationOptions::new("task_complete", "Done"),
            TerminalNotificationChannel::Kitty,
            Some("kitty"),
        );
        assert_eq!(
            terminal_notification_escape_sequences(
                &kitty,
                &TerminalEscapeEnvironment {
                    terminal: Some("kitty"),
                    ..Default::default()
                },
                7,
            ),
            vec![
                "\x1b]99;i=7:d=0:p=title;nyan\x1b\\".to_string(),
                "\x1b]99;i=7:p=body;Done\x1b\\".to_string(),
                "\x1b]99;i=7:d=1:a=focus;\x1b\\".to_string(),
            ]
        );

        let iterm2_with_bell = TerminalNotificationRequest::plan(
            TerminalNotificationOptions::new("task_complete", "Done"),
            TerminalNotificationChannel::ITerm2WithBell,
            Some("iTerm.app"),
        );
        assert_eq!(
            terminal_notification_escape_sequences(
                &iterm2_with_bell,
                &TerminalEscapeEnvironment {
                    tmux: true,
                    ..Default::default()
                },
                7,
            ),
            vec![
                "\x1bPtmux;\x1b\x1b]9;\n\nDone\x07\x1b\\".to_string(),
                "\x07".to_string(),
            ]
        );
    }

    #[test]
    fn terminal_progress_escape_sequence_matches_official_osc_9_4_bytes_without_emitting() {
        let env = TerminalEscapeEnvironment::default();

        assert_eq!(
            terminal_progress_escape_sequence(
                TerminalProgressPlan {
                    action: TerminalProgressAction::Set { percentage: 42 },
                },
                &env,
            ),
            "\x1b]9;4;1;42\x07"
        );
        assert_eq!(
            terminal_progress_escape_sequence(
                TerminalProgressPlan {
                    action: TerminalProgressAction::Clear,
                },
                &env,
            ),
            "\x1b]9;4;0;\x07"
        );
        assert_eq!(
            terminal_progress_escape_sequence(
                TerminalProgressPlan {
                    action: TerminalProgressAction::Indeterminate,
                },
                &TerminalEscapeEnvironment {
                    terminal: Some("kitty"),
                    ..Default::default()
                },
            ),
            "\x1b]9;4;3;\x1b\\"
        );
    }

    #[test]
    fn terminal_progress_escape_sequence_wraps_for_tmux_and_screen_like_official() {
        let plan = TerminalProgressPlan {
            action: TerminalProgressAction::Error { percentage: 7 },
        };

        assert_eq!(
            terminal_progress_escape_sequence(
                plan,
                &TerminalEscapeEnvironment {
                    tmux: true,
                    ..Default::default()
                },
            ),
            "\x1bPtmux;\x1b\x1b]9;4;2;7\x07\x1b\\"
        );
        assert_eq!(
            terminal_progress_escape_sequence(
                plan,
                &TerminalEscapeEnvironment {
                    screen: true,
                    ..Default::default()
                },
            ),
            "\x1bP\x1b]9;4;2;7\x07\x1b\\"
        );
    }
}
