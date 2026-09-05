//! Optional wgpu-backed native window renderer.
//!
//! Owned by the `dmac-gpu` agent (see `.claude/agents/`).

// Tests assert; `unwrap`/`expect` there are how a failure is reported.
// In non-test code the workspace lints still forbid them.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
