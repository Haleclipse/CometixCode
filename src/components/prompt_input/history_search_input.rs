//! Maps to: CC `components/PromptInput/HistorySearchInput.tsx`.

use crate::components::text_input::TextInput;
use iocraft::prelude::*;
use unicode_width::UnicodeWidthStr;

/// Maps to: CC `HistorySearchInput` props (`:6-10`). `value` carries both
/// `value` and `onChange`: the TextInput writes the query State it is given,
/// which is `useHistorySearch`'s `historyQuery` / `setHistoryQuery`.
#[derive(Default, Props)]
pub struct HistorySearchInputProps {
    pub value: Option<State<String>>,
    pub history_failed_match: bool,
}

/// Maps to: CC `HistorySearchInput` (`:12-35`).
#[component]
pub fn HistorySearchInput(
    props: &HistorySearchInputProps,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let internal_value = hooks.use_state(String::new);
    let mut cursor = hooks.use_state(|| 0usize);
    let value = props.value.unwrap_or(internal_value);
    // CC :26-28 `cursorOffset={value.length}` with a no-op
    // `onChangeCursorOffset`: "Force cursor to end of search input since
    // navigation should cancel search". The TextInput's cursor is a byte
    // offset, so the end is the query's byte length.
    let end = value.read().len();
    if cursor.get() != end {
        cursor.set(end);
    }
    // CC :29 `columns={stringWidth(value) + 1}`.
    let width = UnicodeWidthStr::width(value.read().as_str()) + 1;
    element! {
        View(flex_direction: FlexDirection::Row, column_gap: 1u32) {
            Text(
                content: if props.history_failed_match { "no matching prompt:" } else { "search prompts:" }.to_string(),
                dim: true,
                wrap: TextWrap::NoWrap,
            )
            TextInput(
                value: value,
                cursor_offset: cursor,
                columns: width,
                focus: true,
                show_cursor: true,
                multiline: false,
                dim_color: true,
            )
        }
    }
}
