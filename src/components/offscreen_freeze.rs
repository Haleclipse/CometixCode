//! Maps to: CC `components/OffscreenFreeze.tsx`.
//!
//! Claude Code's React component freezes a subtree once native terminal
//! scrollback has moved it above the live viewport, and bypasses freezing when
//! `InVirtualListContext` is active. Cometix uses the equivalent iocraft
//! retained-render implementation at the same component boundary.
//!
//! CC's freeze returns a cached element (`OffscreenFreeze.tsx:24-38`), which
//! React still re-renders when a context it reads changes — a ThemeProvider
//! preview or save repaints frozen rows too. iocraft's freeze skips its
//! children entirely, so this boundary hands the resolved theme to iocraft's
//! `refresh_key`: a theme change re-renders the frozen subtree in place,
//! keeping its component instances and their mount-time state (a turn's
//! "still running" summary, say).

pub use iocraft::components::InVirtualListContext;
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct OffscreenFreezeProps<'a> {
    pub children: Vec<AnyElement<'a>>,
    /// See iocraft `OffscreenFreezeProps::terminal_rows`.
    pub terminal_rows: Option<u16>,
    /// See iocraft `OffscreenFreezeProps::damage_on_restore`.
    pub damage_on_restore: Option<bool>,
    /// See iocraft `OffscreenFreezeProps::skip_poll`.
    pub skip_poll: Option<bool>,
}

/// A raw `Component` that lends its children by `iter_mut()`, like every
/// pass-through boundary here (`ThemeProvider`, `KeybindingSetup`).
#[derive(Default)]
pub struct OffscreenFreeze;

impl Component for OffscreenFreeze {
    type Props<'a> = OffscreenFreezeProps<'a>;

    fn new(_props: &Self::Props<'_>) -> Self {
        Self
    }

    fn update(
        &mut self,
        props: &mut Self::Props<'_>,
        mut hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        let hooks = hooks.with_context_stack(updater.component_context_stack());
        let (theme, _) = crate::components::design_system::theme_provider::use_theme(&hooks);
        updater.set_transparent_layout(true);
        let mut frozen = element! {
            iocraft::components::OffscreenFreeze(
                terminal_rows: props.terminal_rows,
                damage_on_restore: props.damage_on_restore,
                skip_poll: props.skip_poll,
                refresh_key: Some(theme as u64),
            ) {
                #(props.children.iter_mut().map(AnyElement::from))
            }
        };
        updater.update_children([&mut frozen], None);
    }
}

#[cfg(test)]
mod tests {
    use super::{InVirtualListContext, OffscreenFreeze};
    use iocraft::prelude::*;

    #[test]
    fn offscreen_freeze_boundary_renders_children() {
        let text = element! {
            OffscreenFreeze(terminal_rows: Some(24u16)) {
                Text(content: "visible child".to_string())
            }
        }
        .render(Some(40))
        .to_string();

        assert!(text.contains("visible child"), "canvas=\n{text}");
    }

    #[test]
    fn offscreen_freeze_exports_virtual_list_context_marker() {
        fn accepts_marker(_: InVirtualListContext) {}
        accepts_marker(InVirtualListContext);
    }
}
