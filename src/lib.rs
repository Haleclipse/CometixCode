//! nyan library crate — module tree mirroring CC `src/`.
//!
//! Binary glue is `src/bin/nyan.rs` (invokes [`entrypoints::cli`]).
//! [`main`] is the port of CC `main.tsx`. `autobins = false` so Cargo does
//! not treat `src/main.rs` as a binary crate root.
//!
//! `special_module_name` must be allowed at crate level — item-level
//! `#[allow(special_module_name)]` on `mod main` does not suppress it.

#![allow(special_module_name)] // `mod main` ≙ CC `main.tsx` at `src/main.rs`

pub mod bootstrap;
pub mod bridge;
pub mod buddy;
pub mod cli;
pub mod commands;
pub mod components;
pub mod constants;
pub mod context;
pub mod coordinator;
pub mod cost_tracker;
pub mod dialog_launchers;
pub mod entrypoints;
pub mod hooks;
pub mod interactive_helpers;
pub mod keybindings;
pub mod main;
pub mod memdir;
pub mod output_styles;
pub mod plugins;
pub mod project_onboarding_state;
pub mod query;
pub mod query_engine;
pub mod repl_launcher;
pub mod schemas;
pub mod screens;
pub mod services;
pub mod skills;
pub mod state;
pub mod task;
pub mod tasks;
pub mod tool;
pub mod tools;
pub mod types;
pub mod utils;
