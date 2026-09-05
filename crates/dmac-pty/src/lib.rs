//! Hosting child processes (shells, Claude, plank) in a PTY.
//!
//! Owned jointly by the `tui-engineer` (rendering, input routing, the toggle)
//! and `session-engineer` (which processes a session hosts, and re-spawning them
//! on reopen) agents. Scaffolding — see `docs/PLAN.md`, milestone M2.
//!
//! The shape this will take:
//!
//! - `portable-pty` to spawn, because it is the only crate that handles Windows
//!   ConPTY properly as well as Unix.
//! - `vt100` (via `tui-term`) as the terminal emulator, so a hosted program's
//!   output becomes a grid we can render into a panel, a floating window, or
//!   fullscreen — the same content in all three, like everything else here.
//! - An escape key that is *never* forwarded to the child, which is what makes
//!   the foreground toggle possible at all.
// Tests assert; `unwrap`/`expect` there are how a failure is reported.
// In non-test code the workspace lints still forbid them.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
