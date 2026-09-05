//! Fuzzy, full-text and semantic search over the VFS.
//!
//! Owned by the `dmac-search` agent (see `.claude/agents/`).

// Tests assert; `unwrap`/`expect` there are how a failure is reported.
// In non-test code the workspace lints still forbid them.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
