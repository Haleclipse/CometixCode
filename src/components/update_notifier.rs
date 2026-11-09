//! Footer row for a newer GitHub Releases version.
//!
//! Display-only: the REPL owns the session-level background check and this
//! component renders the discovered version. It never downloads or installs
//! anything; `nyan update` owns that path.

use crate::utils::auto_updater::{UpdateAvailable, current_version};
use crate::utils::theme::Theme;
use iocraft::prelude::*;

/// Footer height contributed by the notifier: one row when a newer release is
/// known, nothing otherwise.
pub fn update_notifier_row_count(update_available: Option<&UpdateAvailable>) -> usize {
    usize::from(update_available.is_some())
}

#[derive(Default, Props)]
pub struct UpdateNotifierProps {
    /// REPL-owned result, so the row survives PromptInput remounts.
    pub update_available: Option<UpdateAvailable>,
    pub verbose: bool,
}

#[component]
pub fn UpdateNotifier(props: &UpdateNotifierProps, hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<Theme>();
    let Some(available) = props.update_available.as_ref() else {
        return element! { View(width: 0u32, height: 0u32) };
    };

    element! {
        View(flex_direction: FlexDirection::Row, column_gap: 1u32) {
            #(if props.verbose {
                Some(element! {
                    Text(
                        content: format!("current: {} · latest: {}", current_version(), available.version),
                        color: theme.inactive,
                        dim: true,
                        wrap: TextWrap::Truncate,
                    )
                })
            } else {
                None
            })
            Text(
                content: format!("Update available: nyan {} · Run ", available.version),
                color: theme.warning,
                wrap: TextWrap::Truncate,
            )
            Text(
                content: "nyan update".to_string(),
                color: theme.warning,
                weight: Weight::Bold,
                wrap: TextWrap::Truncate,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(props: UpdateNotifierProps) -> String {
        element! {
            ContextProvider(value: Context::owned(*crate::utils::theme::current())) {
                UpdateNotifier(
                    update_available: props.update_available,
                    verbose: props.verbose,
                )
            }
        }
        .render(Some(160))
        .to_string()
    }

    #[test]
    fn renders_nothing_without_an_available_update() {
        let text = render(UpdateNotifierProps::default());
        assert!(text.trim().is_empty(), "canvas=\n{text}");
    }

    #[test]
    fn renders_the_available_version_and_update_command() {
        let text = render(UpdateNotifierProps {
            update_available: Some(UpdateAvailable {
                version: "0.3.0".to_string(),
            }),
            verbose: true,
        });
        assert!(
            text.contains("Update available: nyan 0.3.0"),
            "canvas=\n{text}"
        );
        assert!(text.contains("nyan update"), "canvas=\n{text}");
        assert!(text.contains("current:"), "canvas=\n{text}");
    }
}
