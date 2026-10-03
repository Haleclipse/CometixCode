//! Maps to: CC `hooks/useApiKeyVerification.ts:12-83`.
//!
//! Partial: the canonical `services/api/claude.rs::verify_api_key` transport is
//! still an explicit stub; this hook preserves that error instead of reporting
//! successful verification.

use crate::bootstrap::state::get_is_non_interactive_session;
use crate::services::api::claude::verify_api_key;
use crate::utils::auth::{
    ApiKeySource, GetAnthropicApiKeyOptions, get_anthropic_api_key_with_source,
    get_api_key_from_api_key_helper, is_anthropic_auth_enabled, is_claude_ai_subscriber,
};
use iocraft::prelude::*;
use std::sync::Arc;

/// Maps to: CC `hooks/useApiKeyVerification.ts:12-18` `VerificationStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerificationStatus {
    Loading,
    Valid,
    Invalid,
    Missing,
    Error,
}

impl Default for VerificationStatus {
    fn default() -> Self {
        Self::Loading
    }
}

/// Maps to: CC `hooks/useApiKeyVerification.ts:20-22`
/// `ApiKeyVerificationResult`.
///
/// The private iocraft handler is the stable `useCallback` carrier. Each call
/// starts an independently component-bound future and returns a completion
/// future matching the source callback's `Promise<void>`.
#[derive(Clone)]
pub struct ApiKeyVerificationResult {
    pub status: VerificationStatus,
    pub error: Option<Arc<anyhow::Error>>,
    reverify_handler: Handler<async_channel::Sender<()>>,
}

impl ApiKeyVerificationResult {
    /// Maps to: CC `hooks/useApiKeyVerification.ts:43-78` `verify`, returned as
    /// `reverify` at lines 80-82.
    pub fn reverify(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let (completed_tx, completed_rx) = async_channel::bounded(1);
        (self.reverify_handler)(completed_tx);
        async move {
            let _ = completed_rx.recv().await;
        }
    }
}

/// Maps to: CC `hooks/useApiKeyVerification.ts:24-83`
/// `useApiKeyVerification`.
pub fn use_api_key_verification(hooks: &mut Hooks<'_, '_>) -> ApiKeyVerificationResult {
    let status = hooks.use_state(|| {
        if !is_anthropic_auth_enabled() || is_claude_ai_subscriber() {
            return VerificationStatus::Valid;
        }
        // Avoid executing apiKeyHelper before the trust dialog is shown.
        let api_key = get_anthropic_api_key_with_source(GetAnthropicApiKeyOptions {
            skip_retrieving_key_from_api_key_helper: true,
        });
        if api_key.key.is_some() || api_key.source == ApiKeySource::ApiKeyHelper {
            VerificationStatus::Loading
        } else {
            VerificationStatus::Missing
        }
    });
    let error = hooks.use_state(|| Option::<Arc<anyhow::Error>>::None);
    let current_reverify_handler = hooks.use_async_handler({
        let mut status = status;
        let mut error = error;
        move |completed: async_channel::Sender<()>| async move {
            if !is_anthropic_auth_enabled() || is_claude_ai_subscriber() {
                status.set(VerificationStatus::Valid);
                let _ = completed.send(()).await;
                return;
            }

            // Warm the helper cache, then resolve the canonical source from
            // current process/module state.
            let _ = get_api_key_from_api_key_helper(get_is_non_interactive_session());
            let api_key = get_anthropic_api_key_with_source(GetAnthropicApiKeyOptions::default());
            let Some(api_key) = api_key.key else {
                if api_key.source == ApiKeySource::ApiKeyHelper {
                    status.set(VerificationStatus::Error);
                    error.set(Some(Arc::new(anyhow::anyhow!(
                        "API key helper did not return a valid key"
                    ))));
                } else {
                    status.set(VerificationStatus::Missing);
                }
                let _ = completed.send(()).await;
                return;
            };

            match verify_api_key(&api_key, false).await {
                Ok(true) => status.set(VerificationStatus::Valid),
                Ok(false) => status.set(VerificationStatus::Invalid),
                Err(verify_error) => {
                    error.set(Some(Arc::new(verify_error)));
                    status.set(VerificationStatus::Error);
                }
            }
            let _ = completed.send(()).await;
        }
    });
    // `useCallback(..., [])`: retain the first handler identity while its
    // iocraft task owner remains mounted.
    let reverify_handler = hooks.use_const(move || current_reverify_handler);

    let current_error = error.read().clone();
    ApiKeyVerificationResult {
        status: status.get(),
        error: current_error,
        reverify_handler,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::config::GlobalConfig;
    use crate::utils::test_env::{EnvVarGuard, TEST_ENV_LOCK, TestEnvGuard};
    use futures::StreamExt;
    use std::path::PathBuf;

    const AUTH_ENV_KEYS: &[&str] = &[
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_UNIX_SOCKET",
        "CI",
        "CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR",
        "CLAUDE_CODE_ENTRYPOINT",
        "CLAUDE_CODE_MANAGED_SETTINGS_PATH",
        "CLAUDE_CODE_NON_INTERACTIVE",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
        "CLAUDE_CODE_REMOTE",
        "CLAUDE_CODE_SIMPLE",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_FOUNDRY",
        "CLAUDE_CODE_USE_VERTEX",
        "COMETIX_NON_INTERACTIVE",
        "COMETIX_NON_INTERACTIVE_SESSION",
        "COO_RUNNING_ON_HOMESPACE",
        "NODE_ENV",
        "CLAUDE_CONFIG_DIR",
    ];

    struct CanonicalAuthStateGuard {
        _lock: TestEnvGuard<'static>,
        previous_env: Vec<EnvVarGuard>,
        previous_config: Option<GlobalConfig>,
        previous_interactive: bool,
        previous_cwd: PathBuf,
        previous_original_cwd: PathBuf,
        previous_allowed_sources: Vec<String>,
        previous_flag_settings_path: Option<PathBuf>,
        previous_flag_settings_inline: Option<serde_json::Value>,
        root: PathBuf,
    }

    impl CanonicalAuthStateGuard {
        fn new(config: GlobalConfig) -> Self {
            let lock = TEST_ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let previous_env = AUTH_ENV_KEYS
                .iter()
                .map(|key| EnvVarGuard::unset(*key))
                .collect::<Vec<_>>();
            let previous_cwd = std::env::current_dir().expect("current cwd");
            let previous_original_cwd = crate::bootstrap::state::get_original_cwd();
            let previous_allowed_sources = crate::bootstrap::state::get_allowed_setting_sources();
            let previous_flag_settings_path = crate::bootstrap::state::get_flag_settings_path();
            let previous_flag_settings_inline = crate::bootstrap::state::get_flag_settings_inline();
            let previous_interactive = crate::bootstrap::state::get_is_interactive();
            let previous_config = crate::utils::config::replace_test_global_config(Some(config));
            let root = std::env::temp_dir().join(format!(
                "cometix-use-api-key-verification-{}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&root).expect("create auth test root");

            crate::utils::process_env::set("CLAUDE_CONFIG_DIR", &root);
            crate::utils::process_env::set(
                "CLAUDE_CODE_MANAGED_SETTINGS_PATH",
                root.join("managed.json"),
            );
            std::env::set_current_dir(&root).expect("set auth test cwd");
            crate::bootstrap::state::set_original_cwd(&root);
            crate::bootstrap::state::set_allowed_setting_sources(vec![
                "userSettings".to_string(),
                "projectSettings".to_string(),
                "localSettings".to_string(),
            ]);
            crate::bootstrap::state::set_flag_settings_path(None);
            crate::bootstrap::state::set_flag_settings_inline(None);
            crate::bootstrap::state::set_is_interactive(true);
            crate::bootstrap::state::reset_auth_file_descriptor_caches_for_testing();
            crate::utils::auth::clear_api_key_helper_cache();

            Self {
                _lock: lock,
                previous_env,
                previous_config,
                previous_interactive,
                previous_cwd,
                previous_original_cwd,
                previous_allowed_sources,
                previous_flag_settings_path,
                previous_flag_settings_inline,
                root,
            }
        }

        fn replace_config(&mut self, config: GlobalConfig) {
            let _ = crate::utils::config::replace_test_global_config(Some(config));
        }

        fn write_user_settings(&self, settings: serde_json::Value) {
            std::fs::write(
                self.root.join("settings.json"),
                serde_json::to_vec(&settings).expect("serialize auth test settings"),
            )
            .expect("write auth test settings");
        }
    }

    impl Drop for CanonicalAuthStateGuard {
        fn drop(&mut self) {
            crate::bootstrap::state::set_is_interactive(self.previous_interactive);
            crate::bootstrap::state::set_allowed_setting_sources(
                self.previous_allowed_sources.clone(),
            );
            crate::bootstrap::state::set_flag_settings_path(
                self.previous_flag_settings_path.clone(),
            );
            crate::bootstrap::state::set_flag_settings_inline(
                self.previous_flag_settings_inline.clone(),
            );
            let _ = crate::utils::config::replace_test_global_config(self.previous_config.take());
            let _ = std::env::set_current_dir(&self.previous_cwd);
            crate::bootstrap::state::set_original_cwd(&self.previous_original_cwd);
            drop(std::mem::take(&mut self.previous_env));
            crate::utils::auth::clear_api_key_helper_cache();
            crate::bootstrap::state::reset_auth_file_descriptor_caches_for_testing();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[derive(Default, Props)]
    struct ApiKeyVerificationHarnessProps {
        reverify_on_mount: bool,
    }

    #[component]
    fn ApiKeyVerificationHarness(
        props: &ApiKeyVerificationHarnessProps,
        mut hooks: Hooks,
    ) -> impl Into<AnyElement<'static>> {
        let verification = use_api_key_verification(&mut hooks);
        let reverify_on_mount = props.reverify_on_mount;
        let verification_for_mount = verification.clone();
        hooks.use_effect(
            move || {
                if reverify_on_mount {
                    // Calling reverify starts work before its Promise-equivalent is
                    // discarded, matching `void reverify()` from REPL's mount effect.
                    drop(verification_for_mount.reverify());
                }
            },
            (),
        );
        let status = match verification.status {
            VerificationStatus::Loading => "loading",
            VerificationStatus::Valid => "valid",
            VerificationStatus::Invalid => "invalid",
            VerificationStatus::Missing => "missing",
            VerificationStatus::Error => "error",
        };
        let error = verification
            .error
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        element! { Text(content: format!("{status}|{error}")) }
    }

    fn render_status(reverify_on_mount: bool) -> String {
        element!(ApiKeyVerificationHarness(reverify_on_mount: reverify_on_mount))
            .render(Some(100))
            .to_string()
            .trim()
            .to_string()
    }

    #[test]
    fn use_api_key_verification_initial_state_matches_official_auth_sources() {
        let mut guard = CanonicalAuthStateGuard::new(GlobalConfig::default());
        assert_eq!(render_status(false), "missing|");

        let mut managed_key = GlobalConfig::default();
        managed_key.primary_api_key = Some("managed-key".to_string());
        guard.replace_config(managed_key);
        assert_eq!(render_status(false), "loading|");

        crate::utils::process_env::set("CLAUDE_CODE_USE_BEDROCK", "1");
        assert_eq!(render_status(false), "valid|");
    }

    #[test]
    fn use_api_key_verification_initializer_skips_helper_execution_matches_official() {
        let guard = CanonicalAuthStateGuard::new(GlobalConfig::default());
        guard.write_user_settings(serde_json::json!({
            "apiKeyHelper": "this-helper-must-not-run-during-initialization"
        }));
        crate::utils::process_env::set("CLAUDE_CODE_REMOTE", "1");
        crate::utils::auth::reset_auth_io_probe();

        assert_eq!(render_status(false), "loading|");
        assert_eq!(
            crate::utils::auth::auth_io_probe_snapshot().api_key_helper_subprocesses,
            0
        );
    }

    #[test]
    fn use_api_key_verification_reverify_matches_official_error_state_update() {
        let mut managed_key = GlobalConfig::default();
        managed_key.primary_api_key = Some("managed-key".to_string());
        let _guard = CanonicalAuthStateGuard::new(managed_key);

        let rendered = futures::executor::block_on(async {
            let mut app = element!(ApiKeyVerificationHarness(reverify_on_mount: true));
            let mut render_loop = Box::pin(
                app.mock_terminal_render_loop(MockTerminalConfig::default().with_size(100, 2)),
            );
            let mut last = String::new();
            for _ in 0..8 {
                let next = crate::utils::race(render_loop.next(), async {
                    futures_timer::Delay::new(std::time::Duration::from_millis(100)).await;
                    None
                })
                .await;
                let Some(canvas) = next else {
                    break;
                };
                last = canvas.to_string();
                if last.contains("error|verifyApiKey stubbed") {
                    break;
                }
            }
            last
        });

        assert!(
            rendered.contains("error|verifyApiKey stubbed"),
            "canvas=\n{rendered}"
        );
    }
}
