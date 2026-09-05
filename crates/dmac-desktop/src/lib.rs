//! Launching, finding and raising external GUI windows, bound to sessions.
//!
//! Owned by the `tui-engineer` agent. Scaffolding — see `docs/adr/0003-...` and
//! `docs/PLAN.md` milestone M2.
//!
//! This is the answer to "run many VS Code windows and switch between them from
//! the session list": we never embed another application's window, we ask the
//! window server to bring it forward. That is supported on macOS (behind the
//! Accessibility permission), on Windows, and on X11 — unlike embedding, which
//! is available on neither macOS nor Wayland.
// Tests assert; `unwrap`/`expect` there are how a failure is reported.
// In non-test code the workspace lints still forbid them.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
