//! Maps to: CC `components/ui/OrderedListItem.tsx`:1-23.

use iocraft::prelude::*;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct OrderedListItemContext {
    pub marker: String,
}

#[derive(Default, Props)]
pub struct OrderedListItemProps {
    pub children: Vec<AnyElement<'static>>,
}

#[component]
pub fn OrderedListItem(
    props: &mut OrderedListItemProps,
    hooks: Hooks,
) -> impl Into<AnyElement<'static>> {
    let marker = hooks
        .try_use_context::<OrderedListItemContext>()
        .map(|context| context.marker.clone())
        .unwrap_or_default();
    let body = std::mem::take(&mut props.children);

    element! {
        View(flex_direction: FlexDirection::Row, column_gap: 1u32) {
            Text(content: marker, dim: true, wrap: TextWrap::NoWrap)
            View(flex_direction: FlexDirection::Column) {
                #(body)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::theme;

    #[test]
    fn ordered_list_item_uses_marker_context_like_official() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                ContextProvider(value: Context::owned(OrderedListItemContext { marker: "1.".to_string() })) {
                    OrderedListItem() {
                        Text(content: "First".to_string())
                    }
                }
            }
        }
        .render(Some(80))
        .to_string();

        assert!(text.contains("1."), "canvas=\n{text}");
        assert!(text.contains("First"), "canvas=\n{text}");
    }
}
