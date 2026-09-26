//! Maps to: CC `components/LanguagePicker.tsx`:1-55.
//!
//! The picker owns its free-text `language` / `cursorOffset` state (`:18-21`),
//! edits it through `TextInput`, cancels on the Settings-context `confirm:no`
//! binding (`:23-25`), and hands the trimmed value — `None` when empty — to
//! `on_complete` (`:27-30`). Config mounts it and only reacts to those two
//! callbacks, as CC's Config does.
//!
//! Deliberate Rust-shape detail: the keybinding handler must be `'static`,
//! while `on_cancel` / `on_complete` borrow the parent. Both are relayed
//! through a State and delivered on the next render, the same transport
//! `ModelPicker` uses.

use crate::components::text_input::TextInput;
use crate::constants::figures::MAIN_SYMBOLS;
use crate::keybindings::keybinding_context::KeybindingRuntime;
use crate::keybindings::types::ContextName;
use crate::keybindings::use_keybinding::use_keybinding;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

/// CC `:48` `columns={60}`.
const LANGUAGE_PICKER_COLUMNS: usize = 60;
/// CC `:47` ``placeholder={`e.g., Japanese, 日本語, Español${figures.ellipsis}`}``.
const LANGUAGE_PICKER_PLACEHOLDER: &str = "e.g., Japanese, 日本語, Español…";
/// CC `:52`.
const LANGUAGE_PICKER_DEFAULT_HINT: &str = "Leave empty for default (English)";

/// Maps to: CC `LanguagePicker.tsx`:27-30 `handleSubmit()` —
/// `onComplete(language?.trim() || undefined)`.
fn submitted_language(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[derive(Default, Props)]
pub(crate) struct LanguagePickerProps<'a> {
    /// CC `initialLanguage: string | undefined`.
    pub initial_language: Option<String>,
    /// CC `onComplete: (language: string | undefined) => void`.
    pub on_complete: HandlerMut<'a, Option<String>>,
    /// CC `onCancel: () => void`.
    pub on_cancel: HandlerMut<'a, ()>,
}

/// Maps to: CC `components/LanguagePicker.tsx`:13-55.
#[component]
pub(crate) fn LanguagePicker<'a>(
    props: &mut LanguagePickerProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let theme = *hooks.use_context::<Theme>();
    // CC :18-21. The TextInput cursor is a byte offset, so CC's
    // `(initialLanguage ?? '').length` is the byte length here.
    let initial_language = props.initial_language.clone().unwrap_or_default();
    let initial_cursor = initial_language.len();
    let language = hooks.use_state(move || initial_language);
    let cursor_offset = hooks.use_state(move || initial_cursor);
    let mut should_cancel = hooks.use_state(|| false);
    let mut pending_complete = hooks.use_state(|| None::<Option<String>>);

    // CC :23-25: "Use Settings context so 'n' key doesn't trigger cancel
    // (allows typing 'n' in input)".
    let runtime = hooks
        .try_use_context::<KeybindingRuntime>()
        .map(|runtime| runtime.clone());
    use_keybinding(
        &mut hooks,
        runtime,
        "confirm:no",
        ContextName::Settings,
        || true,
        move || {
            should_cancel.set(true);
            true
        },
    );

    if should_cancel.get() {
        should_cancel.set(false);
        (props.on_cancel)(());
    }
    let completed = pending_complete.read().clone();
    if let Some(language) = completed {
        pending_complete.set(None);
        (props.on_complete)(language);
    }

    element! {
        View(flex_direction: FlexDirection::Column, gap: 1u32) {
            Text(content: "Enter your preferred response and voice language:".to_string())
            View(flex_direction: FlexDirection::Row, gap: 1u32) {
                Text(content: MAIN_SYMBOLS.pointer.to_string(), wrap: TextWrap::NoWrap)
                TextInput(
                    value: Some(language),
                    cursor_offset: Some(cursor_offset),
                    focus: true,
                    show_cursor: true,
                    placeholder: Some(LANGUAGE_PICKER_PLACEHOLDER.to_string()),
                    columns: LANGUAGE_PICKER_COLUMNS,
                    // CC's BaseTextInput does not stop Escape, so the
                    // `confirm:no` binding above still sees it.
                    escape_event_passthrough: true,
                    on_submit: move |value: String| {
                        pending_complete.set(Some(submitted_language(&value)));
                    },
                )
            }
            Text(content: LANGUAGE_PICKER_DEFAULT_HINT.to_string(), color: theme.inactive)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    #[test]
    fn language_picker_submit_trims_and_empties_to_none_like_official() {
        assert_eq!(submitted_language("  日本語  ").as_deref(), Some("日本語"));
        assert_eq!(submitted_language("   "), None);
        assert_eq!(submitted_language(""), None);
    }

    fn render_picker(initial_language: Option<&str>) -> String {
        crate::utils::process_runtime::initialize_test_process_runtime();
        let current_theme = *theme::current();
        let store = crate::state::store::AppStore::new(Default::default(), None);
        element! {
            ContextProvider(value: Context::owned(current_theme)) {
                ContextProvider(value: Context::owned(store)) {
                    LanguagePicker(initial_language: initial_language.map(str::to_string))
                }
            }
        }
        .render(Some(100))
        .to_string()
    }

    #[test]
    fn language_picker_renders_official_prompt_placeholder_and_hint() {
        let text = render_picker(None);
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
    fn language_picker_seeds_the_field_from_initial_language() {
        let text = render_picker(Some("Spanish"));
        assert!(text.contains("Spanish"), "canvas=\n{text}");
        assert!(
            !text.contains("Japanese, 日本語"),
            "placeholder should be hidden once language has text; canvas=\n{text}"
        );
    }
}
