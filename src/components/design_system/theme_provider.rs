//! Maps to: CC `components/design-system/ThemeProvider.tsx`.
//!
//! CC's `ink.ts` wraps every render in a `ThemeProvider` (`ink.ts:12-23`); the
//! port mounts one at the interactive root (`main.rs`) and in the static
//! renderer (`utils/static_render.rs`). The provider publishes two contexts:
//! the resolved [`Theme`] palette every themed component already reads, and a
//! [`ThemeContextValue`] carrying CC's `ThemeContextValue` — the saved setting,
//! the resolved theme name, and the preview/save/cancel controls.
//!
//! React re-renders a context consumer past any `React.memo`. iocraft reads
//! context when a component updates, so a memo boundary that bails keeps the
//! colours it was rendered with; the boundaries on the message path carry the
//! theme name in their keys for that reason (`screens/repl.rs`,
//! `components/messages_list.rs`, `components/message_row.rs`).

use std::sync::Arc;

use crate::utils::system_theme::{self, SystemTheme};
use crate::utils::theme::{self, Theme, ThemeName, ThemeSetting};
use iocraft::prelude::*;

/// Maps to: CC `ThemeProvider.tsx:46-48` `defaultInitialTheme` —
/// `getGlobalConfig().theme`. The port's config keeps the raw string; an absent
/// or unknown value takes the config default (`'dark'`).
pub fn default_initial_theme() -> ThemeSetting {
    crate::utils::config::load_global_config()
        .theme
        .as_deref()
        .and_then(ThemeSetting::from_setting_value)
        .unwrap_or_default()
}

/// CC's own `resolveThemeSetting(getGlobalConfig().theme)` (`LogoV2.tsx:252`,
/// `Stats.tsx:821`, `QueryEngine.ts:363`) — the theme a freshly mounted
/// provider renders with (`:59-68,96-97`). For port code that bakes colours
/// outside a render, where CC would hand a theme key to a component under that
/// provider.
pub fn initial_theme_name() -> ThemeName {
    system_theme::resolve_theme_setting(default_initial_theme())
}

/// Maps to: CC `ThemeProvider.tsx:50-52` `defaultSaveTheme`.
fn default_save_theme(setting: ThemeSetting) {
    if let Err(error) = crate::utils::config::save_global_config(|config| {
        config.theme = Some(setting.setting_value().to_string());
    }) {
        tracing::warn!("failed to save theme setting: {error}");
    }
}

/// CC `onThemeSave?: (setting: ThemeSetting) => void`.
pub type ThemeSaveHandler = Arc<dyn Fn(ThemeSetting) + Send + Sync>;

/// The default `onThemeSave`, allocated once: the provider updates with its
/// root every frame.
fn default_save_handler() -> ThemeSaveHandler {
    static HANDLER: std::sync::OnceLock<ThemeSaveHandler> = std::sync::OnceLock::new();
    HANDLER
        .get_or_init(|| Arc::new(default_save_theme))
        .clone()
}

/// React's state setter bails out on an unchanged value (`Object.is`), while
/// iocraft's `State::set` always schedules an update. State whose provider has
/// unmounted reads `None` and is left alone.
fn set_if_changed<T: Copy + PartialEq + Send + Sync + 'static>(mut state: State<T>, value: T) {
    if state.try_get().is_some_and(|current| current != value) {
        state.set(value);
    }
}

#[derive(Clone)]
struct ThemeControls {
    theme_setting: State<ThemeSetting>,
    preview_theme: State<Option<ThemeSetting>>,
    system_theme: State<SystemTheme>,
    on_theme_save: ThemeSaveHandler,
}

/// Maps to: CC `ThemeProvider.tsx:17-26` `ThemeContextValue`.
///
/// Outside a provider it is CC's default context (`:31-38`): the dark theme and
/// setters that do nothing.
#[derive(Clone)]
pub struct ThemeContextValue {
    theme_setting: ThemeSetting,
    current_theme: ThemeName,
    controls: Option<ThemeControls>,
}

impl Default for ThemeContextValue {
    fn default() -> Self {
        Self {
            theme_setting: ThemeSetting::default(),
            current_theme: ThemeName::Dark,
            controls: None,
        }
    }
}

impl ThemeContextValue {
    /// The saved user preference; may be `Auto`.
    pub fn theme_setting(&self) -> ThemeSetting {
        self.theme_setting
    }

    /// The resolved theme to render with; never `Auto`.
    pub fn current_theme(&self) -> ThemeName {
        self.current_theme
    }

    /// Maps to: CC `ThemeProvider.tsx:102-112` `setThemeSetting`.
    pub fn set_theme_setting(&self, setting: ThemeSetting) {
        let Some(controls) = &self.controls else {
            return;
        };
        set_if_changed(controls.theme_setting, setting);
        set_if_changed(controls.preview_theme, None);
        // Switching to 'auto' restarts the watcher; seed from the cache so the
        // OSC round-trip doesn't flash the wrong palette.
        if setting == ThemeSetting::Auto {
            set_if_changed(controls.system_theme, system_theme::get_system_theme_name());
        }
        (controls.on_theme_save)(setting);
    }

    /// Maps to: CC `ThemeProvider.tsx:113-118` `setPreviewTheme`.
    pub fn set_preview_theme(&self, setting: ThemeSetting) {
        let Some(controls) = &self.controls else {
            return;
        };
        set_if_changed(controls.preview_theme, Some(setting));
        if setting == ThemeSetting::Auto {
            set_if_changed(controls.system_theme, system_theme::get_system_theme_name());
        }
    }

    /// Maps to: CC `ThemeProvider.tsx:119-125` `savePreview`. CC reads the
    /// preview its closure captured at render; the port reads the state when
    /// called, which differs only for a preview set and saved within one batch.
    pub fn save_preview(&self) {
        let Some(controls) = &self.controls else {
            return;
        };
        if let Some(preview) = controls.preview_theme.try_get().flatten() {
            set_if_changed(controls.theme_setting, preview);
            set_if_changed(controls.preview_theme, None);
            (controls.on_theme_save)(preview);
        }
    }

    /// Maps to: CC `ThemeProvider.tsx:126-130` `cancelPreview`.
    pub fn cancel_preview(&self) {
        let Some(controls) = &self.controls else {
            return;
        };
        set_if_changed(controls.preview_theme, None);
    }
}

fn theme_context(hooks: &Hooks) -> ThemeContextValue {
    hooks
        .try_use_context::<ThemeContextValue>()
        .map(|value| value.clone())
        .unwrap_or_default()
}

/// Maps to: CC `ThemeProvider.tsx:143-146` `useTheme` — the resolved theme and
/// the value whose [`ThemeContextValue::set_theme_setting`] is CC's setter.
pub fn use_theme(hooks: &Hooks) -> (ThemeName, ThemeContextValue) {
    let value = theme_context(hooks);
    (value.current_theme, value)
}

/// Maps to: CC `ThemeProvider.tsx:152-154` `useThemeSetting`.
pub fn use_theme_setting(hooks: &Hooks) -> ThemeSetting {
    theme_context(hooks).theme_setting
}

/// Maps to: CC `ThemeProvider.tsx:156-160` `usePreviewTheme` — the value whose
/// `set_preview_theme` / `save_preview` / `cancel_preview` are CC's.
pub fn use_preview_theme(hooks: &Hooks) -> ThemeContextValue {
    theme_context(hooks)
}

#[derive(Default, Props)]
pub struct ThemeProviderProps<'a> {
    pub children: Vec<AnyElement<'a>>,
    /// CC `initialState?: ThemeSetting`; absent reads the config.
    pub initial_state: Option<ThemeSetting>,
    /// CC `onThemeSave = defaultSaveTheme`; absent is the default.
    pub on_theme_save: Option<ThemeSaveHandler>,
}

/// Maps to: CC `ThemeProvider.tsx:54-137` `ThemeProvider`.
///
/// A raw `Component`, not a `#[component]` fn: its children are lent by
/// `iter_mut()` on every update rather than drained, because where the
/// provider is itself the root element (the static renderer, tests) the same
/// stored props are re-borrowed on every frame. The handle context is a
/// `ContextProvider` built around those borrowed children each update, since
/// `update_children` provides one context.
#[derive(Default)]
pub struct ThemeProvider;

impl Component for ThemeProvider {
    type Props<'a> = ThemeProviderProps<'a>;

    fn new(_props: &Self::Props<'_>) -> Self {
        Self
    }

    fn update(
        &mut self,
        props: &mut Self::Props<'_>,
        mut hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        let mut hooks = hooks.with_context_stack(updater.component_context_stack());
        let initial_state = props.initial_state;
        // CC :59-62.
        let theme_setting =
            hooks.use_state(|| initial_state.unwrap_or_else(default_initial_theme));
        let preview_theme = hooks.use_state(|| None::<ThemeSetting>);
        // CC :64-68: track the terminal theme for 'auto'. Seeds from
        // $COLORFGBG (or 'dark'), corrected by the watcher.
        let system_theme = hooks.use_state(|| {
            if initial_state.unwrap_or(theme_setting.get()) == ThemeSetting::Auto {
                system_theme::get_system_theme_name()
            } else {
                SystemTheme::Dark
            }
        });
        // CC :70-71: the preview wins while a picker is open.
        let active_setting = preview_theme.get().unwrap_or(theme_setting.get());
        // CC :75-94 watches the terminal theme while 'auto' is active, behind
        // `feature('AUTO_THEME')`. The 2.1.88 watcher module is a generated
        // stub and needs a terminal querier the component tree does not have
        // yet; until both land, 'auto' stays on the seeded value.
        let current_theme = match active_setting {
            ThemeSetting::Auto => system_theme.get().theme_name(),
            ThemeSetting::Named(name) => name,
        };
        let value = ThemeContextValue {
            theme_setting: theme_setting.get(),
            current_theme,
            controls: Some(ThemeControls {
                theme_setting,
                preview_theme,
                system_theme,
                on_theme_save: props
                    .on_theme_save
                    .clone()
                    .unwrap_or_else(default_save_handler),
            }),
        };
        let palette: Theme = *theme::get_theme(current_theme);

        updater.set_transparent_layout(true);
        let mut scope = element! {
            ContextProvider(value: Context::owned(value)) {
                #(props.children.iter_mut().map(AnyElement::from))
            }
        };
        updater.update_children([&mut scope], Some(Context::owned(palette).borrow()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::sync::Mutex;

    /// Shows what a themed descendant sees: the handle's names, and whether
    /// the palette is the resolved theme's. `text` and `diff_added` together
    /// tell every pair of palettes apart (the daltonized ones share `text`
    /// with their base, the ANSI ones share `diff_added`).
    #[component]
    fn ThemeEcho(hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let palette = *hooks.use_context::<Theme>();
        let (current, value) = use_theme(&hooks);
        let resolved = theme::get_theme(current);
        let label = if (palette.text, palette.diff_added) == (resolved.text, resolved.diff_added) {
            "palette-matches"
        } else {
            "palette-differs"
        };
        element! {
            Text(content: format!(
                "setting={} current={} {label}",
                value.theme_setting().setting_value(),
                current.setting_value(),
            ))
        }
    }

    #[test]
    fn theme_provider_resolves_its_initial_state_for_both_contexts() {
        let text = element! {
            ThemeProvider(
                initial_state: Some(ThemeSetting::Named(ThemeName::LightAnsi)),
                on_theme_save: Some(Arc::new(|_| {}) as ThemeSaveHandler),
            ) {
                ThemeEcho
            }
        }
        .render(Some(80))
        .to_string();
        assert!(
            text.contains("setting=light-ansi current=light-ansi palette-matches"),
            "{text}"
        );
    }

    #[test]
    fn use_theme_outside_a_provider_is_the_official_default_context() {
        // CC ThemeProvider.tsx:28-38: dark, and setters that do nothing.
        #[component]
        fn Bare(hooks: Hooks) -> impl Into<AnyElement<'static>> {
            let (current, value) = use_theme(&hooks);
            value.set_preview_theme(ThemeSetting::Named(ThemeName::Light));
            value.save_preview();
            element! { Text(content: format!("{} {}", current.setting_value(), use_theme_setting(&hooks).setting_value())) }
        }
        let text = element!(Bare).render(Some(40)).to_string();
        assert_eq!(text.trim_end(), "dark dark");
    }

    #[derive(Default, Props)]
    struct PreviewDriverProps {
        saves: Option<Arc<Mutex<Vec<ThemeSetting>>>>,
    }

    /// Drives the provider's controls from keys, the way a picker does:
    /// `p` previews light-ansi, `s` saves the preview, `c` previews
    /// light-daltonized then cancels it, `t` sets dark-daltonized. Every key
    /// also bumps a visible count, so each one is followed by a frame.
    #[component]
    fn PreviewDriver(props: &PreviewDriverProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let value = use_preview_theme(&hooks);
        let handle = value.clone();
        let mut presses = hooks.use_state(|| 0usize);
        hooks.use_terminal_events(move |event| {
            let TerminalEvent::Key(key) = event else {
                return;
            };
            if key.kind != KeyEventKind::Press {
                return;
            }
            presses.set(presses.get() + 1);
            match key.code {
                KeyCode::Char('p') => {
                    handle.set_preview_theme(ThemeSetting::Named(ThemeName::LightAnsi))
                }
                KeyCode::Char('s') => handle.save_preview(),
                KeyCode::Char('c') => {
                    handle.set_preview_theme(ThemeSetting::Named(ThemeName::LightDaltonized));
                    handle.cancel_preview();
                }
                KeyCode::Char('t') => {
                    handle.set_theme_setting(ThemeSetting::Named(ThemeName::DarkDaltonized))
                }
                _ => {}
            }
        });
        let saves = props.saves.clone().expect("saves");
        let saved = saves
            .lock()
            .unwrap()
            .iter()
            .map(|setting| setting.setting_value())
            .collect::<Vec<_>>()
            .join(",");
        element! {
            View(flex_direction: FlexDirection::Column) {
                ThemeEcho
                Text(content: format!("presses={} saved=[{saved}]", presses.get()))
            }
        }
    }

    #[test]
    fn theme_provider_previews_saves_and_cancels_through_its_own_rerender() {
        // CC ThemeProvider.tsx:102-130. Each step re-renders the provider on
        // its own state while its parent stays put, so the lent children must
        // survive every one of those updates. Frame-driven: each step waits
        // for the press count its key produced, then checks the theme there.
        let saves = Arc::new(Mutex::new(Vec::new()));
        let frames = futures::executor::block_on({
            let saves = saves.clone();
            async move {
                let (keys, events) = async_channel::unbounded();
                let record = saves.clone();
                let mut app = element! {
                    ThemeProvider(
                        initial_state: Some(ThemeSetting::Named(ThemeName::Dark)),
                        on_theme_save: Some(Arc::new(move |setting| record.lock().unwrap().push(setting)) as ThemeSaveHandler),
                    ) {
                        PreviewDriver(saves: Some(saves.clone()))
                    }
                };
                let mut render_loop = Box::pin(app.mock_terminal_render_loop(
                    MockTerminalConfig::with_events(events).with_size(80, 4),
                ));
                let press = |c| TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char(c)));
                let keys_in_order = ['p', 's', 'c', 't'];
                let mut frames = Vec::new();
                while let Some(canvas) = render_loop.next().await {
                    let text = canvas.to_string();
                    // Lent children that went missing end the run here, as a
                    // failure, rather than leaving it waiting for a frame.
                    if !text.contains("presses=") {
                        frames.push(text);
                        break;
                    }
                    if !text.contains(&format!("presses={} ", frames.len())) {
                        continue;
                    }
                    frames.push(text);
                    let Some(key) = keys_in_order.get(frames.len() - 1) else {
                        break;
                    };
                    keys.send(press(*key)).await.unwrap();
                }
                frames
            }
        });
        let expected = [
            "setting=dark current=dark palette-matches",
            // p: the preview wins; the saved setting is untouched.
            "setting=dark current=light-ansi palette-matches",
            // s: the preview becomes the setting and is saved.
            "setting=light-ansi current=light-ansi palette-matches",
            // c: a cancelled preview leaves the setting in effect.
            "setting=light-ansi current=light-ansi palette-matches",
            // t: a direct set is saved too.
            "setting=dark-daltonized current=dark-daltonized palette-matches",
        ];
        assert_eq!(frames.len(), expected.len(), "frames={frames:#?}");
        for (frame, expected) in frames.iter().zip(expected) {
            assert!(frame.contains(expected), "expected {expected:?} in\n{frame}");
        }
        assert_eq!(
            *saves.lock().unwrap(),
            vec![
                ThemeSetting::Named(ThemeName::LightAnsi),
                ThemeSetting::Named(ThemeName::DarkDaltonized)
            ]
        );
    }
}
