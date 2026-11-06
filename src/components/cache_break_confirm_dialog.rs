//! Maps to CC `components/CacheBreakConfirmDialog.tsx` (2.1.198 PIt/Jom).
use crate::components::custom_select::{
    Select, SelectInputOptionMeta, SelectOptionData, UseSelectInputOptions, UseSelectStateProps,
    use_select_input, use_select_state,
};
use crate::components::design_system::dialog::Dialog;
use crate::state::store::AppStore;
use crate::utils::effort::Effort;
use iocraft::prelude::*;

#[derive(Default, Props)]
pub struct CacheBreakConfirmDialogProps<'a> {
    pub effort: Option<Effort>,
    pub on_confirm: HandlerMut<'a, ()>,
    pub on_cancel: HandlerMut<'a, ()>,
}

/// Effort branch of CC CacheBreakConfirmDialog; uses the established Select
/// input/state carrier. The model-switch flow is outside this effort change.
#[component]
pub fn CacheBreakConfirmDialog<'a>(
    props: &mut CacheBreakConfirmDialogProps<'a>,
    mut hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let store = hooks
        .try_use_context::<AppStore>()
        .map(|store| store.clone());
    let theme = hooks
        .try_use_context::<crate::utils::theme::Theme>()
        .map(|theme| *theme)
        .unwrap_or_else(|| *crate::utils::theme::current());
    let target = props
        .effort
        .as_ref()
        .map(|effort| effort.as_str().to_string())
        .unwrap_or_else(|| "auto".into());
    let options = vec![
        SelectOptionData {
            label: format!("Yes, switch to {target}"),
            value: "confirm".into(),
            ..Default::default()
        },
        SelectOptionData {
            label: "No, go back".into(),
            value: "cancel".into(),
            ..Default::default()
        },
    ];
    let state = use_select_state(
        &mut hooks,
        UseSelectStateProps {
            visible_option_count: Some(2),
            values: vec!["confirm".into(), "cancel".into()],
            default_value: None,
            focus_value: Some("confirm".into()),
        },
    );
    let events = use_select_input(
        &mut hooks,
        state,
        UseSelectInputOptions {
            has_on_cancel: true,
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
    let mut pending_cancel = hooks.use_state(|| false);
    let accepted = events.take_accepted();
    if accepted.as_deref() == Some("confirm") {
        if let Some(store) = store {
            store.replace_with(|state| {
                state.cache_miss_acked_at_output_tokens =
                    i64::try_from(crate::cost_tracker::get_total_output_tokens())
                        .unwrap_or(i64::MAX);
            });
        }
        (props.on_confirm)(());
    } else if accepted.as_deref() == Some("cancel")
        || events.take_cancelled()
        || pending_cancel.get()
    {
        pending_cancel.set(false);
        (props.on_cancel)(());
    }
    let focused_index = usize::from(state.focused_value().as_deref() == Some("cancel"));
    let mut target_segment = StyledSegment::new(&target);
    target_segment.styles.bold = Some(true);
    element! {
        Dialog(
            title: "Change effort level?".to_string(),
            subtitle: Some("Your next response will be slower and use more tokens".to_string()),
            color: Some(theme.warning), hide_input_guide: true,
            on_cancel: move |_| pending_cancel.set(true),
        ) {
            View(flex_direction: FlexDirection::Column, gap: 1, margin_bottom: 1u32) {
                Text(segments: Some(vec![
                    StyledSegment::new("This conversation is cached for the current effort level. Switching to "),
                    target_segment,
                    StyledSegment::new(" means the full history gets re-read on your next message."),
                ]), wrap: TextWrap::Wrap)
                Select(options: options, focused_index: focused_index, visible_option_count: 2usize)
            }
        }
    }
}
