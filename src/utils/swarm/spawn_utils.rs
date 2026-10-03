//! Shared teammate spawn utilities.
//! Maps to: CC `utils/swarm/spawnUtils.ts`.
//!
//! `buildInheritedCliFlags` exists TWICE in CC and the two copies diverge.
//! This file owns `spawnUtils.ts:38-87`, whose ONLY caller is
//! `PaneBackendExecutor.ts:130` (verified: the only other import of this module
//! is `spawnMultiAgent.ts:52`, which takes `buildInheritedEnvVars` alone). It
//! appends `--teammate-mode` (`:77-78`) and has NO `auto` branch. The
//! module-local copy at `spawnMultiAgent.ts:208-260` — used by both spawn
//! handlers at `:418`/`:625` — does the opposite, and is ported next to its
//! declaration in `tools/shared/spawn_multi_agent.rs`. Do not merge them.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::types::permissions::PermissionMode;
use crate::utils::process_env::JsTruthy;
use crate::utils::swarm::backends::teammate_mode_snapshot::get_teammate_mode_from_snapshot;
use crate::utils::swarm::constants::TEAMMATE_COMMAND_ENV_VAR;

/// Maps to: CC `utils/swarm/spawnUtils.ts#TEAMMATE_ENV_VARS`.
pub const TEAMMATE_ENV_VARS: &[&str] = &[
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "ANTHROPIC_BASE_URL",
    "CLAUDE_CONFIG_DIR",
    "CLAUDE_CODE_REMOTE",
    "CLAUDE_CODE_REMOTE_MEMORY_DIR",
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "NO_PROXY",
    "no_proxy",
    "SSL_CERT_FILE",
    "NODE_EXTRA_CA_CERTS",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
];

pub(crate) fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    if value
        .bytes()
        .all(|byte| matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.' | b'/' | b':' | b'@' | b'+' | b'=' | b','))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Maps to: CC `getTeammateCommand()` pure selection rules.
pub fn get_teammate_command_from_parts(
    teammate_command_env: Option<&str>,
    current_executable: impl Into<PathBuf>,
) -> String {
    if let Some(command) = teammate_command_env.truthy() {
        return command.to_string();
    }
    current_executable.into().display().to_string()
}

/// Maps to: CC `getTeammateCommand()`.
pub fn get_teammate_command() -> String {
    let executable = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("cometix"));
    get_teammate_command_from_parts(
        crate::utils::process_env::var(TEAMMATE_COMMAND_ENV_VAR).as_deref(),
        executable,
    )
}

/// Maps to: CC `buildInheritedCliFlags(...)` input options.
///
/// CC's parameter object is only `{ planModeRequired?, permissionMode? }`; the
/// other five fields are read from `bootstrap/state.ts` module state inside the
/// function body. This struct widens those reads into inputs so both copies of
/// the function stay testable without process-global state. Shared by
/// [`build_inherited_cli_flags_with_options`] here and by the module-local copy
/// in `tools/shared/spawn_multi_agent.rs`: it is a Rust-only carrier for an
/// anonymous TS object type, not a CC-declared symbol, so it has no owning file
/// under the declaration contract.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuildInheritedCliFlagsOptions {
    pub plan_mode_required: bool,
    pub permission_mode: Option<PermissionMode>,
    pub session_bypass_permissions_mode: bool,
    pub model_override: Option<String>,
    pub settings_path: Option<PathBuf>,
    pub inline_plugins: Vec<PathBuf>,
    pub chrome_flag_override: Option<bool>,
}

/// Maps to: CC `spawnUtils.ts:38-87#buildInheritedCliFlags`.
///
/// The `PaneBackendExecutor` copy: no `auto` branch, and `--teammate-mode` is
/// appended (`:77-78`). See the module doc for the spawnMultiAgent twin.
pub fn build_inherited_cli_flags_with_options(options: BuildInheritedCliFlagsOptions) -> String {
    let mut flags = Vec::new();

    if !options.plan_mode_required {
        if options.permission_mode == Some(PermissionMode::BypassPermissions)
            || options.session_bypass_permissions_mode
        {
            flags.push("--dangerously-skip-permissions".to_string());
        } else if let Some(PermissionMode::AcceptEdits) = options.permission_mode {
            flags.push("--permission-mode acceptEdits".to_string());
        }
        // CC `spawnUtils.ts:49-56` has no `auto` arm — deliberately. The
        // spawnMultiAgent copy (`:226-231`) does.
    }

    if let Some(model_override) = options.model_override.filter(|value| !value.is_empty()) {
        flags.push(format!("--model {}", shell_quote(&model_override)));
    }

    if let Some(settings_path) = options.settings_path {
        flags.push(format!(
            "--settings {}",
            shell_quote(&settings_path.display().to_string())
        ));
    }

    for plugin_dir in options.inline_plugins {
        flags.push(format!(
            "--plugin-dir {}",
            shell_quote(&plugin_dir.display().to_string())
        ));
    }

    flags.push(format!(
        "--teammate-mode {}",
        get_teammate_mode_from_snapshot().as_str()
    ));

    if let Some(chrome) = options.chrome_flag_override {
        flags.push(if chrome { "--chrome" } else { "--no-chrome" }.to_string());
    }

    flags.join(" ")
}

/// Maps to: CC `buildInheritedCliFlags(...)`.
///
/// Rust does not yet carry every official CLI override in `bootstrap/state.ts`
/// (notably inline plugin dirs and chrome flag override). This function keeps
/// the official boundary and threads the state CometixCode currently stores.
pub fn build_inherited_cli_flags(
    plan_mode_required: bool,
    permission_mode: Option<PermissionMode>,
) -> String {
    build_inherited_cli_flags_with_options(BuildInheritedCliFlagsOptions {
        plan_mode_required,
        permission_mode,
        settings_path: crate::bootstrap::state::get_flag_settings_path(),
        ..Default::default()
    })
}

/// Maps to: CC `buildInheritedEnvVars()`.
pub fn build_inherited_env_vars_from_map(env: &BTreeMap<String, String>) -> String {
    let mut env_vars = vec![
        "CLAUDECODE=1".to_string(),
        "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1".to_string(),
    ];

    for key in TEAMMATE_ENV_VARS {
        if let Some(value) = env.get(*key).filter(|value| !value.is_empty()) {
            env_vars.push(format!("{key}={}", shell_quote(value)));
        }
    }

    env_vars.join(" ")
}

/// Maps to: CC `buildInheritedEnvVars()`, which reads `process.env` — here the
/// environment carrier, so settings-applied variables reach the teammate.
pub fn build_inherited_env_vars() -> String {
    let env = crate::utils::process_env::snapshot()
        .iter()
        .filter_map(|(key, value)| Some((key.to_str()?.to_string(), value.to_str()?.to_string())))
        .collect::<BTreeMap<_, _>>();
    build_inherited_env_vars_from_map(&env)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_teammate_command_prefers_override_like_official() {
        assert_eq!(
            get_teammate_command_from_parts(Some("/custom/claude"), "/bin/cometix"),
            "/custom/claude"
        );
        assert_eq!(
            get_teammate_command_from_parts(None, "/bin/cometix"),
            "/bin/cometix"
        );
    }

    #[test]
    fn build_inherited_env_vars_forwards_official_provider_and_proxy_subset() {
        let mut env = BTreeMap::new();
        env.insert("CLAUDE_CODE_USE_VERTEX".to_string(), "1".to_string());
        env.insert(
            "ANTHROPIC_BASE_URL".to_string(),
            "https://example.test/with space".to_string(),
        );
        env.insert("UNRELATED".to_string(), "ignored".to_string());

        let rendered = build_inherited_env_vars_from_map(&env);
        assert!(rendered.starts_with("CLAUDECODE=1 CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1"));
        assert!(rendered.contains("CLAUDE_CODE_USE_VERTEX=1"));
        assert!(rendered.contains("ANTHROPIC_BASE_URL='https://example.test/with space'"));
        assert!(!rendered.contains("UNRELATED"));
    }

    #[test]
    fn build_inherited_cli_flags_respects_plan_mode_and_teammate_mode() {
        let _lock = crate::utils::swarm::backends::registry::TEST_BACKEND_REGISTRY_LOCK
            .lock()
            .unwrap();
        crate::utils::swarm::backends::teammate_mode_snapshot::reset_teammate_mode_snapshot_for_test();
        crate::utils::swarm::backends::teammate_mode_snapshot::set_cli_teammate_mode_override(
            crate::utils::swarm::backends::teammate_mode_snapshot::TeammateMode::Tmux,
        );

        let flags = build_inherited_cli_flags_with_options(BuildInheritedCliFlagsOptions {
            plan_mode_required: false,
            permission_mode: Some(PermissionMode::AcceptEdits),
            model_override: Some("claude model".to_string()),
            settings_path: Some(PathBuf::from("/tmp/settings.json")),
            inline_plugins: vec![PathBuf::from("/tmp/plugin dir")],
            chrome_flag_override: Some(false),
            ..Default::default()
        });
        assert!(flags.contains("--permission-mode acceptEdits"));
        assert!(flags.contains("--model 'claude model'"));
        assert!(flags.contains("--settings /tmp/settings.json"));
        assert!(flags.contains("--plugin-dir '/tmp/plugin dir'"));
        assert!(flags.contains("--teammate-mode tmux"));
        assert!(flags.contains("--no-chrome"));

        let plan_flags = build_inherited_cli_flags_with_options(BuildInheritedCliFlagsOptions {
            plan_mode_required: true,
            permission_mode: Some(PermissionMode::BypassPermissions),
            ..Default::default()
        });
        assert!(!plan_flags.contains("--dangerously-skip-permissions"));
        assert!(plan_flags.contains("--teammate-mode tmux"));
        crate::utils::swarm::backends::teammate_mode_snapshot::reset_teammate_mode_snapshot_for_test();
    }

    #[test]
    fn external_permission_mode_helper_still_matches_permission_flag_copy() {
        assert_eq!(
            crate::utils::permissions::permission_mode::to_external_permission_mode(
                PermissionMode::AcceptEdits
            ),
            "acceptEdits"
        );
    }
}
