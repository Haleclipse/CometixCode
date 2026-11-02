//! Maps to: CC `components/design-system/Ratchet.tsx`.
//! Main-screen-safe height ratchet seam. The official component measures its
//! child and locks to the largest observed height; Rust callers can keep that
//! state with `ratchet_next_min_height` and pass the resulting `min_height`.

use iocraft::prelude::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RatchetLock {
    #[default]
    Always,
    Offscreen,
}

pub fn ratchet_next_min_height(current: u32, measured: u32, rows: u32) -> u32 {
    current.max(measured.min(rows))
}

#[derive(Default, Props)]
pub struct RatchetProps {
    pub lock: RatchetLock,
    pub min_height: Option<u32>,
    pub is_visible: bool,
    pub children: Vec<AnyElement<'static>>,
}

#[component]
pub fn Ratchet(props: &mut RatchetProps) -> impl Into<AnyElement<'static>> {
    let engaged = matches!(props.lock, RatchetLock::Always)
        || (matches!(props.lock, RatchetLock::Offscreen) && !props.is_visible);
    let children = std::mem::take(&mut props.children);

    element! {
        View(
            flex_direction: FlexDirection::Column,
            min_height: if engaged { props.min_height.unwrap_or(0) } else { 0u32 },
        ) {
            #(children)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratchet_helper_keeps_largest_observed_height_capped_to_rows() {
        let mut height = 0;
        height = ratchet_next_min_height(height, 4, 10);
        height = ratchet_next_min_height(height, 2, 10);
        height = ratchet_next_min_height(height, 20, 10);

        assert_eq!(height, 10);
    }

    #[test]
    fn ratchet_renders_children_with_main_screen_min_height() {
        let canvas = element! {
            Ratchet(min_height: Some(3u32)) {
                Text(content: "row".to_string())
            }
        }
        .render(Some(20));

        assert!(canvas.height() >= 3, "canvas=\n{}", canvas);
        assert!(canvas.to_string().contains("row"));
    }
}
