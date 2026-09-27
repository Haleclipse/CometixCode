//! Maps to: CC `components/HelpV2/General.tsx`.

use crate::components::prompt_input::prompt_input_help_menu::PromptInputHelpMenu;
use iocraft::prelude::*;

/// Maps to CC `components/HelpV2/General.tsx:5-22#General`.
#[component]
pub fn General(_hooks: Hooks) -> impl Into<AnyElement<'static>> {
    element! {
        View(flex_direction: FlexDirection::Column, padding_top: 1u32, padding_bottom: 1u32, row_gap: 1u32) {
            View {
                Text(content: "Cometix Code understands your codebase, makes edits with your permission, and executes commands — right from your terminal.".to_string())
            }
            View(flex_direction: FlexDirection::Column) {
                View {
                    Text(content: "Shortcuts".to_string(), weight: Weight::Bold)
                }
                PromptInputHelpMenu(gap: 2u32, fixed_width: true, dim_color: false)
            }
        }
    }
}
