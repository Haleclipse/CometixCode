//! `/provider` slash command registration boundary.

pub mod provider;

pub use provider::{call, ARGUMENT_HINT, DESCRIPTION, NAME};

use crate::commands::Command;

/// Returns the `/provider` command descriptor.
pub fn command() -> Command {
    Command::local(NAME, DESCRIPTION)
        .argument_hint(ARGUMENT_HINT)
        .supports_non_interactive()
        .executable(call)
}
