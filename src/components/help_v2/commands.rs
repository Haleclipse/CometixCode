//! Maps to: CC `components/HelpV2/Commands.tsx`.

use std::collections::HashSet;
use std::sync::Arc;

use crate::commands::Command;
use crate::components::custom_select::{
    DisableSelection, Select, SelectInputOptionMeta, SelectLayout, SelectOptionData,
    UseSelectInputOptions, UseSelectStateProps, use_select_input, use_select_state,
};
use crate::components::design_system::tabs::use_tab_header_focus;
use crate::utils::theme::Theme;
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct CommandsProps<'a> {
    pub commands: Arc<Vec<Command>>,
    /// CC `maxHeight`.
    pub max_height: u16,
    pub columns: u16,
    pub title: String,
    /// CC `onCancel`.
    pub on_cancel: HandlerMut<'a, ()>,
    pub empty_message: Option<String>,
}

/// Maps to CC `components/HelpV2/Commands.tsx:18-71#Commands`.
///
/// The list owns its keys: its Select hands focus back to the tab row on
/// ↑ from the first item and cancels to `onCancel`, and is disabled while
/// the tab row has focus. CC opts into header focus above its empty-message
/// branch, so on an empty tab ↓ still takes focus off the tab row with no
/// list to hand it back; the port keeps that. The Select renders only
/// outside that branch; there, where CC mounts none, `is_disabled` keeps
/// its keys off and no `onCancel` keeps it from registering the `select`
/// overlay (use-select-input.ts:101).
#[component]
pub fn Commands<'a>(props: &mut CommandsProps<'a>, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let theme = *hooks.use_context::<Theme>();
    let focus = use_tab_header_focus(&mut hooks);
    let max_width = usize::from(props.columns).saturating_sub(10).max(1);
    let visible_count = (usize::from(props.max_height).saturating_sub(10) / 2).max(1);

    // CC :30-45 `options`: deduped by name, sorted by `localeCompare`.
    let mut seen = HashSet::new();
    let mut commands = props
        .commands
        .iter()
        .filter(|command| seen.insert(command.name.to_string()))
        .collect::<Vec<_>>();
    commands.sort_by(|left, right| {
        crate::tools::grep_tool::javascript_locale_compare(&left.name, &right.name)
    });
    let options = commands
        .into_iter()
        .map(|command| SelectOptionData {
            label: format!("/{}", command.name),
            value: command.name.to_string(),
            description: Some(crate::utils::truncate::truncate(
                &crate::commands::format_description_with_source(command),
                max_width,
                true,
            )),
            dim_description: true,
            ..SelectOptionData::default()
        })
        .collect::<Vec<_>>();
    let shows_empty_message = props.commands.is_empty() && props.empty_message.is_some();

    // CC :56-65 `<Select visibleOptionCount onCancel disableSelection
    // hideIndexes layout="compact-vertical" onUpFromFirstItem
    // isDisabled={headerFocused}>`.
    let state = use_select_state(
        &mut hooks,
        UseSelectStateProps {
            visible_option_count: Some(visible_count),
            values: options.iter().map(|option| option.value.clone()).collect(),
            default_value: None,
            focus_value: None,
        },
    );
    let events = use_select_input(
        &mut hooks,
        state,
        UseSelectInputOptions {
            is_disabled: focus.header_focused || shows_empty_message,
            disable_selection: DisableSelection::Yes,
            has_on_cancel: !shows_empty_message,
            has_on_up_from_first_item: true,
            option_metas: options
                .iter()
                .map(|option| SelectInputOptionMeta {
                    value: option.value.clone(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        },
    );
    let cancelled = events.take_cancelled();
    let up_from_first_item = events.take_up_from_first_item();
    if cancelled {
        (props.on_cancel)(());
    }
    if up_from_first_item {
        focus.focus_header();
    }
    let navigation = state.navigation.snapshot();

    element! {
        View(flex_direction: FlexDirection::Column, padding_top: 1u32, padding_bottom: 1u32) {
            #(if shows_empty_message {
                // CC :51 `<Text dimColor>`.
                element! {
                    Text(content: props.empty_message.clone().unwrap_or_default(), color: theme.inactive)
                }.into_any()
            } else {
                element! {
                    Fragment {
                        Text(content: props.title.clone())
                        View(margin_top: 1u32) {
                            Select(
                                options: options,
                                focused_index: navigation.focused_index().unwrap_or(0),
                                visible_from_index: navigation.visible_from_index,
                                visible_option_count: navigation.visible_option_count,
                                layout: SelectLayout::CompactVertical,
                                hide_indexes: true,
                                is_disabled: focus.header_focused,
                            )
                        }
                    }
                }.into_any()
            })
        }
    }
}
