//! Virtual filesystem: local, archives, SFTP, S3 and friends, all as directories.
//!
//! One trait, many backends. The panel above never learns which one it is talking
//! to — that is the whole point. A `.tar.gz` inside a zip on an SFTP host must
//! browse exactly like `/home`.
//!
//! Owned by the `vfs-engineer` agent.

// Tests assert; `unwrap`/`expect` there are how a failure is reported.
// In non-test code the workspace lints still forbid them.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
pub mod local;
pub mod path;

pub use path::{Scheme, VfsPath};

use dmac_core::Entry;
use std::sync::Arc;

/// What a backend can actually do. The UI greys out impossible actions instead
/// of letting the user set up a move and fail at the last moment — an S3 bucket
/// has no `rename`, and pretending otherwise is how data gets lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub read: bool,
    pub write: bool,
    pub rename: bool,
    pub delete: bool,
    pub create_dir: bool,
    pub symlinks: bool,
    pub unix_permissions: bool,
    /// Seekable writes. Object stores do not have them, so an in-place edit of a
    /// 2GB file must be refused rather than silently rewritten.
    pub random_write: bool,
}

impl Capabilities {
    /// A conservative starting point: readable and nothing else. Backends opt in
    /// to each capability explicitly, so a new backend is safe before it is complete.
    pub const READ_ONLY: Self = Self {
        read: true,
        write: false,
        rename: false,
        delete: false,
        create_dir: false,
        symlinks: false,
        unix_permissions: false,
        random_write: false,
    };
}

#[derive(Debug, thiserror::Error)]
pub enum VfsError {
    #[error("{path}: not found")]
    NotFound { path: String },

    #[error("{path}: permission denied")]
    PermissionDenied { path: String },

    #[error("{backend} cannot {operation}")]
    Unsupported {
        backend: &'static str,
        operation: &'static str,
    },

    #[error("{path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("operation cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, VfsError>;

/// A chunk of a directory listing. Listings stream so a directory with a million
/// entries paints its first screen immediately instead of after a full scan.
#[derive(Debug)]
pub struct ListChunk {
    pub entries: Vec<Entry>,
    /// `true` on the final chunk. Until then the panel shows a live count.
    pub complete: bool,
}

/// Everything that can pretend to be a directory tree.
///
/// Every method is `async` and every one must abort promptly when its future is
/// dropped — a hung SFTP mount must not freeze the UI.
#[async_trait::async_trait]
pub trait VfsBackend: Send + Sync + std::fmt::Debug {
    /// Stable identifier used in error messages and the capability matrix.
    fn name(&self) -> &'static str;

    fn capabilities(&self) -> Capabilities;

    /// List a directory, sending chunks as they are discovered. The channel is
    /// bounded by the caller: if the UI cannot keep up, the walk applies
    /// backpressure rather than buffering a million entries into memory.
    async fn list(
        &self,
        path: &VfsPath,
        tx: tokio::sync::mpsc::Sender<Result<ListChunk>>,
    ) -> Result<()>;

    /// Metadata for a single entry.
    async fn stat(&self, path: &VfsPath) -> Result<Entry>;

    /// Whole-file read. For anything that could be large, prefer a streaming
    /// reader — this exists for config files and previews with a known small size.
    async fn read(&self, path: &VfsPath, max_bytes: u64) -> Result<Vec<u8>>;
}

pub type BackendRef = Arc<dyn VfsBackend>;
