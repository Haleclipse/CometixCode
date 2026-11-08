//! Maps to: CC `components/KeybindingWarnings.tsx`.
//!
//! Safety/runtime boundary: official `KeybindingWarnings` reads
//! `isKeybindingCustomizationEnabled()`, `getCachedKeybindingWarnings()`, and
//! `getKeybindingsPath()` from `keybindings/loadUserBindings.js`. Nyan does
//! not yet have the custom keybinding loader/runtime state, so this component
//! preserves the official render boundary from explicit snapshot props only. No
//! settings reads, filesystem reads, or keybinding validation side effects occur
//! here; the future keybinding-loader slice should pass the real snapshot.

pub use crate::keybindings::types::KeybindingWarningSeverity;
pub use crate::keybindings::validate::KeybindingWarning as KeybindingWarningItem;
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct KeybindingWarningsProps {
    /// Maps to CC `isKeybindingCustomizationEnabled()`.
    pub enabled: bool,
    /// Maps to CC `getKeybindingsPath()`.
    pub keybindings_path: String,
    /// Maps to CC `getCachedKeybindingWarnings()`.
    pub warnings: Vec<KeybindingWarningItem>,
}

pub fn keybinding_warnings_should_render(enabled: bool, warnings_len: usize) -> bool {
    enabled && warnings_len > 0
}

/// Maps to: CC `components/KeybindingWarnings.tsx` `KeybindingWarnings`.
#[component]
pub fn KeybindingWarnings(
    props: &KeybindingWarningsProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();

    if !keybinding_warnings_should_render(props.enabled, props.warnings.len()) {
        return element! { View {} };
    }

    let errors = props
        .warnings
        .iter()
        .filter(|warning| warning.severity == KeybindingWarningSeverity::Error)
        .cloned()
        .collect::<Vec<_>>();
    let warns = props
        .warnings
        .iter()
        .filter(|warning| warning.severity == KeybindingWarningSeverity::Warning)
        .cloned()
        .collect::<Vec<_>>();
    let title_color = if errors.is_empty() {
        theme.warning
    } else {
        theme.error
    };

    element! {
        View(flex_direction: FlexDirection::Column, margin_top: 1u32, margin_bottom: 1u32) {
            Text(content: "Keybinding Configuration Issues".to_string(), bold: true, color: title_color, wrap: TextWrap::NoWrap)
            View(flex_direction: FlexDirection::Row) {
                Text(content: "Location: ".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                Text(content: props.keybindings_path.clone(), color: theme.inactive, wrap: TextWrap::NoWrap)
            }
            View(margin_left: 1u32, margin_top: 1u32, flex_direction: FlexDirection::Column) {
                #(errors.into_iter().map(|warning| element! {
                    KeybindingWarningRow(warning: warning)
                }))
                #(warns.into_iter().map(|warning| element! {
                    KeybindingWarningRow(warning: warning)
                }))
            }
        }
    }
}

#[derive(Default, Props)]
struct KeybindingWarningRowProps {
    warning: KeybindingWarningItem,
}

#[component]
fn KeybindingWarningRow(
    props: &KeybindingWarningRowProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = hooks.use_context::<crate::utils::theme::Theme>();
    let (label, color) = match props.warning.severity {
        KeybindingWarningSeverity::Error => ("[Error]", theme.error),
        KeybindingWarningSeverity::Warning => ("[Warning]", theme.warning),
    };
    let suggestion = props.warning.suggestion.clone();

    element! {
        View(flex_direction: FlexDirection::Column) {
            View(flex_direction: FlexDirection::Row) {
                Text(content: "└ ".to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                Text(content: label.to_string(), color: color, wrap: TextWrap::NoWrap)
                Text(content: format!(" {}", props.warning.message), color: theme.inactive, wrap: TextWrap::NoWrap)
            }
            #(suggestion.map(|suggestion| element! {
                View(margin_left: 3u32) {
                    Text(content: format!("→ {suggestion}"), color: theme.inactive, wrap: TextWrap::NoWrap)
                }
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    fn render(enabled: bool, warnings: Vec<KeybindingWarningItem>) -> String {
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                KeybindingWarnings(
                    enabled: enabled,
                    keybindings_path: "/home/me/.nyan/keybindings.json".to_string(),
                    warnings: warnings,
                )
            }
        }
        .render(Some(120))
        .to_string()
    }

    #[test]
    fn keybinding_warnings_visibility_matches_official_gate() {
        assert!(!keybinding_warnings_should_render(false, 1));
        assert!(!keybinding_warnings_should_render(true, 0));
        assert!(keybinding_warnings_should_render(true, 1));

        let disabled = render(
            false,
            vec![KeybindingWarningItem::error("bad binding", None::<String>)],
        );
        assert!(disabled.trim().is_empty(), "canvas=\n{disabled}");

        let empty = render(true, Vec::new());
        assert!(empty.trim().is_empty(), "canvas=\n{empty}");
    }

    #[test]
    fn keybinding_warnings_render_errors_before_warnings_like_official() {
        let text = render(
            true,
            vec![
                KeybindingWarningItem::warning("unused action", Some("remove it")),
                KeybindingWarningItem::error("invalid key", Some("use ctrl+x")),
            ],
        );

        assert!(
            text.contains("Keybinding Configuration Issues"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains("Location: /home/me/.nyan/keybindings.json"),
            "canvas=\n{text}"
        );
        let error_pos = text.find("[Error] invalid key").expect("error row");
        let warning_pos = text.find("[Warning] unused action").expect("warning row");
        assert!(error_pos < warning_pos, "canvas=\n{text}");
        assert!(text.contains("→ use ctrl+x"), "canvas=\n{text}");
        assert!(text.contains("→ remove it"), "canvas=\n{text}");
    }
}
