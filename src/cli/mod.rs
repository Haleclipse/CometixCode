//! Structured CLI surface — Maps to: CC `main.tsx` commander + `cli/` wiring.
//!
//! All argv categories land in [`config::CliConfig`]. Incomplete paths short-
//! circuit via [`dispatch`] with `Unimplemented: <name>` (stderr, exit 1).

pub mod config;
pub mod dispatch;
pub mod parse;
pub mod print;
/// CC `cli/structuredIO.ts`. Private to `cli`: `print` is its only consumer,
/// mirroring `cli/print.ts` being the only file that constructs a `StructuredIO`.
mod structured_io;
pub mod web;

pub use config::{CliConfig, ImplStatus};
pub use dispatch::dispatch;
pub use parse::parse_cli_config;
