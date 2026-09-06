//! Core domain: entries, panels, jobs and the file-operation engine.
//!
//! This crate is the single source of truth. It knows nothing about terminals,
//! GPUs or language models — everything above it renders a snapshot of this
//! state and sends back intents. That is what lets the TUI backend, the GPU
//! backend and a future headless/RPC mode share one implementation.
//!
//! Owned by the `rust-architect` and `fileops-engineer` agents.

// Tests assert; `unwrap`/`expect` there are how a failure is reported.
// In non-test code the workspace lints still forbid them.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
pub mod clipboard;
pub mod complete;
pub mod entry;
pub mod fuzzy;
pub mod history;
pub mod panel;
pub mod tools;

pub use entry::{Entry, EntryKind, SortKey, SortOrder};
pub use panel::{Panel, PanelId};

/// Errors that escape the core layer.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("operation cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, Error>;
