//! What can go wrong, said precisely.
//!
//! Every variant here exists because the alternative — an `io::Error` shown as
//! "Os error 28" — makes the user guess. The rule for this module is that the
//! message names the *file* and the *thing that failed*, in that order, because
//! a job over 100,000 entries produces failures that have to be readable in a
//! list.

use std::io;
use std::path::{Path, PathBuf};

/// A failure that ends a job.
///
/// Per-entry failures are not these — see [`super::Failure`]. A job aborts only
/// when continuing would be meaningless (the destination is gone) or dangerous
/// (nobody is left to answer a conflict, so we would have to guess whether to
/// overwrite).
#[derive(Debug, thiserror::Error)]
pub enum FileOpsError {
    #[error("{path}: {action} failed: {source}")]
    Io {
        path: PathBuf,
        action: &'static str,
        #[source]
        source: io::Error,
    },

    #[error("{0}")]
    Name(#[from] super::names::NameError),

    #[error("{path} is inside {source_path}: a copy into its own subtree never ends")]
    DestinationInsideSource { path: PathBuf, source_path: PathBuf },

    #[error("{0} is both the source and the destination")]
    SameFile(PathBuf),

    #[error("{0} is a filesystem root and will not be deleted")]
    RootPath(PathBuf),

    #[error("nothing was selected")]
    NoSources,

    #[error("{0} is not a directory")]
    NotADirectory(PathBuf),

    #[error(
        "a conflict on {0} went unanswered — the job stopped rather than guess whether to overwrite"
    )]
    Unanswered(PathBuf),

    #[error("the job was aborted at a conflict on {0}")]
    AbortedAtConflict(PathBuf),

    #[error("cancelled")]
    Cancelled,
}

impl FileOpsError {
    /// Attach the path and the verb to an `io::Error`. Used everywhere rather
    /// than `?` on a bare `io::Result`, because "permission denied" without a
    /// path is not a report, it is a riddle.
    pub(crate) fn io(path: impl AsRef<Path>, action: &'static str, source: io::Error) -> Self {
        Self::Io {
            path: path.as_ref().to_path_buf(),
            action,
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, FileOpsError>;

/// The shape of a per-entry failure, for a UI that wants to group 50,000 of
/// them into "47 permission denied, 3 in use by another program".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    PermissionDenied,
    NotFound,
    /// The disk filled up. The only failure that makes continuing pointless.
    OutOfSpace,
    /// Windows: another process holds the file with no share-delete. There is
    /// no Unix equivalent, which is exactly why it needs naming — a Unix
    /// developer will not think of it.
    Locked,
    ReadOnlyFilesystem,
    /// The name cannot exist on this platform. Reported, never silently mangled.
    IllegalName,
    /// The source changed underneath the copy. For a move this is fatal to that
    /// entry: the source is never deleted after it.
    SourceChanged,
    /// A `.dmac-part` file was already there. Either a crashed run or another
    /// job — indistinguishable, so neither is overwritten.
    PartialInTheWay,
    Other,
}

impl FailureKind {
    /// Classify an `io::Error`. Windows' sharing violation has no `ErrorKind`,
    /// so it is matched on the raw code — one of the few places where a magic
    /// number is the honest option.
    pub(crate) fn of(e: &io::Error) -> Self {
        #[cfg(windows)]
        {
            const ERROR_SHARING_VIOLATION: i32 = 32;
            const ERROR_LOCK_VIOLATION: i32 = 33;
            if matches!(
                e.raw_os_error(),
                Some(ERROR_SHARING_VIOLATION) | Some(ERROR_LOCK_VIOLATION)
            ) {
                return FailureKind::Locked;
            }
        }
        match e.kind() {
            io::ErrorKind::PermissionDenied => FailureKind::PermissionDenied,
            io::ErrorKind::NotFound => FailureKind::NotFound,
            io::ErrorKind::StorageFull => FailureKind::OutOfSpace,
            io::ErrorKind::ReadOnlyFilesystem => FailureKind::ReadOnlyFilesystem,
            io::ErrorKind::InvalidFilename => FailureKind::IllegalName,
            io::ErrorKind::ResourceBusy => FailureKind::Locked,
            _ => FailureKind::Other,
        }
    }

    /// Whether hitting this once means every remaining entry will hit it too.
    /// A full disk is not worth discovering 99,000 more times.
    pub fn is_terminal(self) -> bool {
        matches!(self, FailureKind::OutOfSpace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_message_names_the_path_and_the_verb() {
        let e = FileOpsError::io(
            "/tmp/x",
            "open",
            io::Error::from(io::ErrorKind::PermissionDenied),
        );
        let s = e.to_string();
        assert!(s.contains("/tmp/x"), "{s}");
        assert!(s.contains("open"), "{s}");
    }

    #[test]
    fn a_full_disk_stops_the_job_and_permission_denied_does_not() {
        assert!(FailureKind::of(&io::Error::from(io::ErrorKind::StorageFull)).is_terminal());
        assert!(!FailureKind::of(&io::Error::from(io::ErrorKind::PermissionDenied)).is_terminal());
    }

    #[test]
    fn io_kinds_map_to_something_a_user_can_read() {
        assert_eq!(
            FailureKind::of(&io::Error::from(io::ErrorKind::NotFound)),
            FailureKind::NotFound
        );
        assert_eq!(
            FailureKind::of(&io::Error::from(io::ErrorKind::ReadOnlyFilesystem)),
            FailureKind::ReadOnlyFilesystem
        );
        assert_eq!(
            FailureKind::of(&io::Error::other("something new")),
            FailureKind::Other
        );
    }
}
