//! Thin interactive App — Maps to: CC `components/App.tsx` (providers only).
//!
//! Provider tree: [`crate::state::app_state::AppStateProvider`] (CC
//! `App.tsx:28-33`) plus REPL diagnostics/runtime contexts around the final
//! [`crate::screens::repl::Repl`]. Theme/keybinding framework providers are
//! mounted once by the retained root in [`crate::main`].
//!
//! The store and the startup settings snapshot arrive as typed props (CC
//! `Props.initialState`), assembled in [`crate::main`] and threaded through
//! [`crate::repl_launcher`] / [`crate::dialog_launchers`]. There is no disk
//! fallback and no ambient store context read: Contract B clause 2 makes
//! `AppStateProvider` the SOLE source of the `AppStore` context.
//! Setup gates complete in [`crate::main`] before this component is mounted.
//! MCP/LSP/statusline startup futures live in [`crate::main`] (CC `main.tsx`).
//!
//! Capability grades: `docs/ENTRY_OWNERSHIP_AUDIT.md`.

use crate::interactive_helpers::default_status_notice_context;
use crate::main::build_startup_diagnostics_snapshot;
use crate::state::app_state::{AppStateProvider, ProviderChildren};
use crate::state::store::AppStore;
use crate::utils::settings::SettingsWithErrors;
use iocraft::prelude::*;
use std::sync::Arc;

/// Cloneable retained-mode mapping of React's re-renderable `children` value.
/// iocraft elements are move-only, so the factory rebuilds the render value on
/// each AppStore-driven render while reconciliation preserves child hook state.
#[derive(Clone)]
pub struct AppChildren(Arc<dyn Fn() -> AnyElement<'static> + Send + Sync>);

impl AppChildren {
    pub fn new(render: impl Fn() -> AnyElement<'static> + Send + Sync + 'static) -> Self {
        Self(Arc::new(render))
    }

    fn render(&self) -> AnyElement<'static> {
        (self.0)()
    }
}

impl Default for AppChildren {
    fn default() -> Self {
        Self::new(|| element! { Fragment }.into_any())
    }
}

/// Maps to: CC `components/App.tsx` `Props` (`children` + `initialState`).
///
/// `Default` is iocraft's `Props` requirement, not a production fallback: every
/// production mount ([`crate::repl_launcher::launch_repl`],
/// [`crate::dialog_launchers::launch_resume_chooser`]) passes both values.
#[derive(Default, Props)]
pub struct AppProps {
    pub children: AppChildren,
    /// Maps to: CC `App.tsx:11` `Props.initialState` → `:28-30
    /// <AppStateProvider initialState=…>`. Rust builds the store during the
    /// retained launch phase (`main.rs`), so the store itself is threaded down
    /// and adopted by the provider (`L1 (Prebuilt interactive AppStore
    /// provider adoption)`).
    ///
    /// **Required.** CC's `App.tsx:11` declares `initialState: AppState`
    /// without `?` — it is `AppStateProvider.initialState` (`:51`) that is
    /// optional, not App's. The `Option` here exists solely because iocraft
    /// `Props` must derive `Default`; `None` is rejected at mount rather than
    /// silently creating a store, so App has no fallback store (Contract B
    /// clause 2).
    pub app_store: Option<AppStore>,
    /// Startup settings snapshot resolved before mount (CC `main.tsx`
    /// `getSettingsWithAllErrors()`); App never reads settings off disk during
    /// a frame.
    pub startup_settings: Arc<SettingsWithErrors>,
}

#[component]
pub fn App(props: &AppProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let external_editor_runtime =
        crate::utils::prompt_editor::ExternalEditorRuntime::new(hooks.use_app());
    let agent_generator_runtime = hooks
        .use_const(crate::components::agents::generate_agent::production_agent_generator_runtime);

    let startup_settings = props.startup_settings.clone();

    let runtime_config_snapshot = hooks.use_const({
        let startup_settings = startup_settings.clone();
        move || build_startup_diagnostics_snapshot(&startup_settings)
    });

    let status_notice_context = hooks.use_const({
        let runtime_config_snapshot = runtime_config_snapshot.clone();
        move || {
            default_status_notice_context(runtime_config_snapshot.ide_installation_status.as_ref())
        }
    });

    // Maps to: CC `App.tsx:28-33` — the AppState provider wraps `children`.
    // FpsMetricsProvider (:26) / StatsProvider (:27) are diagnostics layers
    // with no Rust counterpart yet; their position is above this provider.
    // Every other App-scoped context stays inside it, so descendants keep the
    // same lookup order while the store context now has exactly one provider.
    let children = props.children.clone();
    // CC `App.tsx:11` requires `initialState`; the launch phase always builds
    // the store, so a missing one is a wiring bug, not a fallback path.
    let app_store = props.app_store.clone().expect(
        "App requires a prebuilt AppStore — CC App.tsx:11 `initialState` is required and the \
         retained launch phase always builds one",
    );
    element! {
        AppStateProvider(
            prebuilt_store: Some(app_store),
            // CC `App.tsx:30` — the interactive root is the provider that gets
            // the change pipeline; auxiliary dialog providers get none.
            on_change_app_state: Some(crate::state::on_change_app_state::default_on_change()),
            children: ProviderChildren::new(move || {
                element! {
                    ContextProvider(value: Context::owned((*runtime_config_snapshot).clone())) {
                        View {
                            ContextProvider(value: Context::owned(status_notice_context.clone())) {
                                ContextProvider(value: Context::owned(external_editor_runtime)) {
                                    ContextProvider(value: Context::owned(agent_generator_runtime.clone())) {
                                        #(children.render())
                                    }
                                }
                            }
                        }
                    }
                }
                .into_any()
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{StreamExt, stream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    static APP_CHILD_MOUNTS: AtomicUsize = AtomicUsize::new(0);

    #[component]
    fn AppChildProbe(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let _mount = hooks.use_state(|| {
            APP_CHILD_MOUNTS.fetch_add(1, Ordering::SeqCst);
        });
        let verbose = crate::state::app_state::use_app_state(&mut hooks, |state| state.verbose);
        element! { Text(content: format!("child marker {verbose}")) }
    }

    #[test]
    fn app_preserves_official_children_across_store_updates() {
        APP_CHILD_MOUNTS.store(0, Ordering::SeqCst);
        let startup_settings = Arc::new(SettingsWithErrors {
            settings: crate::utils::settings::SettingsJson::default(),
            errors: Vec::new(),
        });
        let store = crate::state::store::AppStore::new(
            crate::main::build_initial_app_state(
                &startup_settings.settings,
                &startup_settings.errors,
                true,
                &crate::cli::CliConfig::default(),
            )
            .unwrap(),
            None,
        );
        let mut app = element! {
            App(
                app_store: Some(store.clone()),
                startup_settings: startup_settings,
                children: AppChildren::new(|| element! { AppChildProbe }.into_any()),
            )
        };
        let mut render_loop = Box::pin(app.mock_terminal_render_loop(
            MockTerminalConfig::with_events(stream::empty()).with_size(80, 10),
        ));

        let first = futures::executor::block_on(render_loop.next()).expect("initial frame");
        assert!(first.to_string().contains("child marker false"));

        store.replace_with(|state| state.verbose = !state.verbose);
        let second = futures::executor::block_on(crate::utils::race(render_loop.next(), async {
            futures_timer::Delay::new(Duration::from_secs(1)).await;
            None
        }));
        let second = second.expect("AppStore update should rerender App");
        assert!(second.to_string().contains("child marker true"));
        assert_eq!(APP_CHILD_MOUNTS.load(Ordering::SeqCst), 1);
    }
}
