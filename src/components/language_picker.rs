//! Maps to: CC `components/LanguagePicker.tsx`:11-55.
//!
//! Official LanguagePicker owns React input state and submit/cancel callbacks.
//! This Rust boundary keeps rendering pure: Settings owns text editing events,
//! while this component renders the official prompt, pointer/TextInput shape,
//! placeholder, and default-language hint.

use crate::constants::figures::MAIN_SYMBOLS;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

pub(crate) const LANGUAGE_PICKER_COLUMNS: u32 = 60;
pub(crate) const LANGUAGE_PICKER_PLACEHOLDER: &str = "e.g., Japanese, 日本語, Español…";
pub(crate) const LANGUAGE_PICKER_DEFAULT_HINT: &str = "Leave empty for default (English)";

pub(crate) fn language_display_to_input(value: &str) -> String {
    if value.eq_ignore_ascii_case("Default (English)") {
        String::new()
    } else {
        value.to_string()
    }
}

/// Maps to: CC `components/LanguagePicker.tsx`:30-33 `handleSubmit()`.
pub(crate) fn language_input_to_display(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        "Default (English)".to_string()
    } else {
        trimmed.to_string()
    }
}

fn cursor_parts(text: &str, cursor_offset: usize) -> (String, String, String) {
    let len = text.chars().count();
    let offset = cursor_offset.min(len);
    let before: String = text.chars().take(offset).collect();
    match text.chars().nth(offset) {
        Some(ch) => {
            let after: String = text.chars().skip(offset + 1).collect();
            (before, ch.to_string(), after)
        }
        None => (before, " ".to_string(), String::new()),
    }
}

#[derive(Default, Props)]
pub(crate) struct LanguagePickerProps {
    pub language: String,
    pub cursor_offset: usize,
    pub columns: Option<u32>,
}

/// Maps to: CC `components/LanguagePicker.tsx`:35-54 render tree.
#[component]
pub(crate) fn LanguagePicker(
    props: &LanguagePickerProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = *hooks.use_context::<Theme>();
    // CC renders the field through TextInput, whose cursor inversion is
    // `isTerminalFocused && !accessibilityEnabled` (TextInput.tsx:82).
    let can_show_cursor = crate::components::text_input::text_input_can_show_cursor(
        hooks.use_terminal_focus(),
        crate::components::text_input::accessibility_enabled_from_env(),
    );
    let language_is_empty = props.language.is_empty();
    let (language_before, language_cursor, language_after) =
        cursor_parts(&props.language, props.cursor_offset);
    let language_placeholder_first = LANGUAGE_PICKER_PLACEHOLDER
        .chars()
        .next()
        .unwrap_or(' ')
        .to_string();
    let language_placeholder_rest: String = LANGUAGE_PICKER_PLACEHOLDER.chars().skip(1).collect();
    let columns = props.columns.unwrap_or(LANGUAGE_PICKER_COLUMNS);

    element! {
        View(flex_direction: FlexDirection::Column) {
            Text(content: "Enter your preferred response and voice language:".to_string())
            View(flex_direction: FlexDirection::Row, margin_top: 1u32) {
                View(margin_right: 1u32) {
                    Text(content: MAIN_SYMBOLS.pointer.to_string(), wrap: TextWrap::NoWrap)
                }
                View(flex_direction: FlexDirection::Row, width: columns, overflow: Overflow::Hidden, height: 1u32) {
                    // CC renderPlaceholder: cursor on the first placeholder
                    // character while the cursor can show, the whole
                    // placeholder dim otherwise.
                    #(if language_is_empty && can_show_cursor {
                        Some(element! {
                            Text(content: language_placeholder_first.clone(), invert: true, wrap: TextWrap::NoWrap)
                        })
                    } else { None })
                    #(if language_is_empty && can_show_cursor {
                        Some(element! {
                            Text(content: language_placeholder_rest.clone(), color: theme.inactive, wrap: TextWrap::NoWrap)
                        })
                    } else { None })
                    #(if language_is_empty && !can_show_cursor {
                        Some(element! {
                            Text(content: LANGUAGE_PICKER_PLACEHOLDER.to_string(), color: theme.inactive, wrap: TextWrap::NoWrap)
                        })
                    } else { None })
                    #(if !language_is_empty {
                        Some(element! {
                            Text(content: language_before.clone(), wrap: TextWrap::NoWrap)
                        })
                    } else { None })
                    #(if !language_is_empty {
                        Some(element! {
                            Text(content: language_cursor.clone(), invert: can_show_cursor, wrap: TextWrap::NoWrap)
                        })
                    } else { None })
                    #(if !language_is_empty {
                        Some(element! {
                            Text(content: language_after.clone(), wrap: TextWrap::NoWrap)
                        })
                    } else { None })
                }
            }
            View(margin_top: 1u32) {
                Text(content: LANGUAGE_PICKER_DEFAULT_HINT.to_string(), color: theme.inactive)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    fn render_picker(language: &str, cursor_offset: usize) -> String {
        let current_theme = *theme::current();
        element! {
            ContextProvider(value: Context::owned(current_theme)) {
                LanguagePicker(
                    language: language.to_string(),
                    cursor_offset: cursor_offset,
                    columns: Some(LANGUAGE_PICKER_COLUMNS),
                )
            }
        }
        .render(Some(100))
        .to_string()
    }

    #[test]
    fn language_picker_submit_helper_trims_and_defaults_like_official() {
        assert_eq!(language_input_to_display("  日本語  "), "日本語");
        assert_eq!(language_input_to_display("   "), "Default (English)");
        assert_eq!(language_display_to_input("Default (English)"), "");
        assert_eq!(language_display_to_input("Spanish"), "Spanish");
    }

    #[test]
    fn language_picker_renders_official_prompt_placeholder_and_hint() {
        let text = render_picker("", 0);
        assert!(
            text.contains("Enter your preferred response and voice language:"),
            "canvas=\n{text}"
        );
        assert!(text.contains("❯"), "canvas=\n{text}");
        assert!(
            text.contains("Japanese, 日本語, Español…"),
            "canvas=\n{text}"
        );
        assert!(
            text.contains(LANGUAGE_PICKER_DEFAULT_HINT),
            "canvas=\n{text}"
        );
    }

    #[test]
    fn language_picker_renders_input_with_cursor_cell() {
        let text = render_picker("Spanish", 3);
        assert!(text.contains("Spanish"), "canvas=\n{text}");
        assert!(
            !text.contains("Japanese, 日本語"),
            "placeholder should be hidden once language has text; canvas=\n{text}"
        );
    }
}
