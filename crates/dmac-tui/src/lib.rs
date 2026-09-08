//! The ratatui terminal user interface.
//!
//! Owned by the `tui-engineer` agent. The rules it works under are in
//! `.claude/agents/tui-engineer.md`; the short version is: nothing blocks the
//! event loop, every keybinding is data, and a crash always restores the terminal.

// Tests assert; `unwrap`/`expect` there are how a failure is reported.
// In non-test code the workspace lints still forbid them.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
pub mod action;
pub mod app;
pub mod help;
pub mod keymap;
pub mod mcp;
pub mod terminal;
pub mod theme;
pub mod tour;
pub(crate) mod ui;
pub mod utilities;

pub use app::run;
