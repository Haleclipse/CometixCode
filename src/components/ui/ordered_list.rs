//! Maps to: CC `components/ui/OrderedList.tsx`:1-47.

use super::ordered_list_item::OrderedListItemContext;
use iocraft::prelude::*;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct OrderedListContext {
    marker: String,
}

#[derive(Default, Props)]
pub struct OrderedListProps {
    pub children: Vec<AnyElement<'static>>,
}

#[component]
pub fn OrderedList(props: &mut OrderedListProps, hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let parent_marker = hooks
        .try_use_context::<OrderedListContext>()
        .map(|context| context.marker.clone())
        .unwrap_or_default();
    let children = std::mem::take(&mut props.children);
    let max_marker_width = children.len().max(1).to_string().len();

    element! {
        View(flex_direction: FlexDirection::Column) {
            #(children.into_iter().enumerate().map(|(index, child)| {
                let padded_marker = format!("{:>width$}.", index + 1, width = max_marker_width);
                let marker = format!("{parent_marker}{padded_marker}");
                element! {
                    ContextProvider(value: Context::owned(OrderedListContext { marker: marker.clone() })) {
                        ContextProvider(value: Context::owned(OrderedListItemContext { marker })) {
                            #(vec![child])
                        }
                    }
                }
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::ui::OrderedListItem;
    use crate::utils::theme;

    fn render_ordered_list() -> String {
        element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                OrderedList() {
                    OrderedListItem() {
                        Text(content: "Alpha".to_string())
                    }
                    OrderedListItem() {
                        Text(content: "Beta".to_string())
                    }
                }
            }
        }
        .render(Some(80))
        .to_string()
    }

    #[test]
    fn ordered_list_numbers_items_like_official_component() {
        let text = render_ordered_list();
        assert!(text.contains("1."), "canvas=\n{text}");
        assert!(text.contains("2."), "canvas=\n{text}");
        assert!(text.contains("Alpha"), "canvas=\n{text}");
        assert!(text.contains("Beta"), "canvas=\n{text}");
    }

    #[test]
    fn ordered_list_pads_marker_width_for_double_digit_lists() {
        let text = element! {
            ContextProvider(value: Context::owned(*theme::current())) {
                OrderedList() {
                    #((0..10usize).map(|index| element! {
                        OrderedListItem() {
                            Text(content: format!("Item {index}"))
                        }
                    }))
                }
            }
        }
        .render(Some(120))
        .to_string();

        assert!(text.contains(" 1."), "canvas=\n{text}");
        assert!(text.contains("10."), "canvas=\n{text}");
    }
}
