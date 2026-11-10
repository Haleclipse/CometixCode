//! Maps to: CC `services/tips/` — spinner tip registry + scheduler.

pub mod tip_history;
pub mod tip_registry;
pub mod tip_scheduler;
pub mod types;

pub use tip_scheduler::{
    get_tip_to_show_on_spinner, pick_new_spinner_tip, record_shown_tip, reset_tip_picked_this_turn,
};
pub use types::Tip;
