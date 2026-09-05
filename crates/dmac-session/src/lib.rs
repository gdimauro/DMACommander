//! Named sessions: persistent workspaces and reattachable agent sessions.
//!
//! Owned by the `dmac-session` agent (see `.claude/agents/`).

// Tests assert; `unwrap`/`expect` there are how a failure is reported.
// In non-test code the workspace lints still forbid them.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
