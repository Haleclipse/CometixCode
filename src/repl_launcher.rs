//! Maps to: CC `replLauncher.tsx`.
//!
//! CC awaits `showSetupScreens(...)` in `main.tsx` before calling
//! `launchRepl(...)`. The retained setup phase therefore lives in `main.rs`;
//! this module owns only the provider-only [`App`] launch.

use crate::components::app::{App, AppChildren};
use crate::screens::repl::ReplProps;
use crate::state::store::AppStore;
use crate::utils::settings::SettingsWithErrors;
use iocraft::prelude::*;
use std::sync::Arc;

/// Maps to: CC `replLauncher.tsx#launchRepl`.
///
/// Scoped contexts carry shared descendant services; immutable REPL launch
/// input remains a direct, strongly typed prop and never enters AppState.
pub fn launch_repl(
    app_store: AppStore,
    startup_settings: Arc<SettingsWithErrors>,
    repl_props: ReplProps,
) -> AnyElement<'static> {
    let children = AppChildren::new(move || repl_props.clone().into_element());
    // Maps to: CC `replLauncher.tsx` `<App initialState={…}>` — the store and
    // the settings snapshot are typed props, not ambient contexts.
    element! {
        App(
            app_store: Some(app_store),
            startup_settings: startup_settings,
            children: children,
        )
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test_env::TEST_ENV_LOCK;
    use futures::{StreamExt, stream};
    use std::fs;
    use std::time::Duration;

    async fn drive_launch(
        repl_props: ReplProps,
        events: Vec<TerminalEvent>,
        max_frames: usize,
    ) -> String {
        let _env_guard = TEST_ENV_LOCK.lock().unwrap();
        let previous_cwd = std::env::current_dir().unwrap();
        let isolated_cwd =
            std::env::temp_dir().join(format!("cometix-repl-launcher-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&isolated_cwd).expect("create isolated launcher cwd");
        std::env::set_current_dir(&isolated_cwd).expect("set isolated launcher cwd");
        crate::utils::config::set_test_global_config(Some(
            crate::utils::config::GlobalConfig::default(),
        ));
        crate::utils::config::reset_trust_dialog_accepted_cache_for_testing();
        // Skip Trust/ClaudeMd gates while mounting the post-setup launcher.
        crate::utils::process_env::set("CLAUBBIT", "1");

        let startup_settings = Arc::new(SettingsWithErrors {
            settings: crate::utils::settings::SettingsJson::default(),
            errors: Vec::new(),
            policy_settings: None,
        });
        let initial = crate::main::build_initial_app_state(
            &startup_settings.settings,
            &startup_settings.errors,
            true,
            &crate::cli::CliConfig::default(),
        )
        .unwrap();
        let store = AppStore::new(initial, None);
        let launch = launch_repl(store, startup_settings, repl_props);
        let keybinding_runtime =
            crate::keybindings::keybinding_context::KeybindingRuntime::with_default_bindings();
        let current_theme = *crate::utils::theme::current();
        let mut app = element! {
            ContextProvider(value: Context::owned(keybinding_runtime)) {
                ContextProvider(value: Context::owned(current_theme)) {
                    #(launch)
                }
            }
        };
        let mut render_loop = Box::pin(app.mock_terminal_render_loop(
            MockTerminalConfig::with_events(stream::iter(events)).with_size(120, 30),
        ));
        let mut last = String::new();
        for _ in 0..max_frames {
            let next = crate::utils::race(render_loop.next(), async {
                futures_timer::Delay::new(Duration::from_millis(150)).await;
                None
            })
            .await;
            let Some(canvas) = next else {
                break;
            };
            last = canvas.to_string();
        }

        std::env::set_current_dir(previous_cwd).expect("restore launcher cwd");
        let _ = fs::remove_dir_all(isolated_cwd);
        crate::utils::config::set_test_global_config(None);
        crate::utils::process_env::remove("CLAUBBIT");
        last
    }

    #[test]
    fn launch_repl_forwards_debug_prop_like_official() {
        // Mirror the process runtime published by the production entrypoint.
        crate::utils::process_runtime::initialize_test_process_runtime();
        let canvas = futures::executor::block_on(drive_launch(
            ReplProps {
                debug: true,
                ..ReplProps::default()
            },
            Vec::new(),
            8,
        ));
        assert!(canvas.contains("Debug mode"), "canvas=\n{canvas}");
    }
}
