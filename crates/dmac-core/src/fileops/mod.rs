//! The file operation engine: the part of this program that can destroy work.
//!
//! # The one rule
//!
//! **Nothing is deleted until its replacement is confirmed to exist and to be
//! correct.** A move within one filesystem is a `rename` — atomic, nothing to
//! confirm. A move across filesystems is copy, flush, re-read, compare, *then*
//! unlink, in that order, per file. There is no option to skip the comparison,
//! because the person who would turn it off is not the person who would lose
//! the file.
//!
//! # Shape
//!
//! A [`Job`] describes what the user asked for. [`spawn`] runs it on its own
//! thread and hands back a [`JobHandle`] and a channel of [`JobEvent`]s. The
//! engine never calls into the UI and knows nothing about it; the UI drains the
//! channel between frames and answers the occasional question. Nothing here
//! blocks a render.
//!
//! ```no_run
//! # use dmac_core::fileops::{Job, Options, JobEvent, Resolution, ConflictChoice, spawn};
//! # use std::path::PathBuf;
//! let job = Job::Copy {
//!     sources: vec![PathBuf::from("/tmp/a.txt")],
//!     destination: PathBuf::from("/tmp/out"),
//! };
//! let (handle, mut events) = spawn(job, Options::default())?;
//! while let Some(event) = events.blocking_recv() {
//!     match event {
//!         JobEvent::Progress(p) => { let _ = p.percent(); }
//!         JobEvent::Conflict(prompt) => {
//!             prompt.answer(Resolution::all(ConflictChoice::Skip));
//!         }
//!         JobEvent::Finished(outcome) => { let _ = outcome.status; }
//!         _ => {}
//!     }
//! }
//! handle.cancel();
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! # What each invariant is defending against
//!
//! | Invariant | The bug it prevents |
//! |---|---|
//! | Bytes land in `name.dmac-part` and are `rename`d into place | a truncated file under a real name after a crash |
//! | Moves force hash verification | deleting an original whose copy was short |
//! | The engine never resolves a conflict itself | a silent overwrite of something the user wanted |
//! | Symlinks are copied as links | a 4 KB link into `/` becoming a copy of the disk |
//! | Only `remove_dir` on source directories | a "move" deleting files it never copied |
//! | The trash never falls back to permanent deletion | "I can get that back" turning out to be false |
//! | Root paths are refused before any `unlink` | one upstream bug emptying a disk |
//!
//! # Cancellation, precisely
//!
//! Checked before every entry and between every chunk within a file, so a
//! cancel is felt in well under a second even on a 100 GB copy, and it works
//! while a conflict dialog is open. What it leaves behind is exactly
//! describable: every file the job reported as done is complete and verified;
//! the file that was in flight is removed with its part file; nothing was
//! deleted whose copy was not already confirmed.
//!
//! A `kill -9` is different — nothing can run then. It leaves the in-flight
//! `.dmac-part` file on disk, which [`is_partial`] recognises so the app can
//! explain it instead of showing the user something inexplicable.
//!
//! # Deliberately not here yet
//!
//! Resuming a job across a restart (the `.dmac-part` convention is the
//! groundwork, but no manifest is written), an undo stack, ACLs, btime, and
//! Windows alternate data streams. Each is named in the code that would have
//! needed it, so it is a gap rather than a surprise.

mod conflict;
mod engine;
mod error;
mod event;
mod file;
mod meta;
mod names;
mod remove;

#[cfg(test)]
mod tests;

pub use conflict::{
    Conflict, ConflictChoice, ConflictKind, ConflictPrompt, Resolution, Side, StandingChoice,
};
pub use error::{FailureKind, FileOpsError};
pub use event::{Failure, JobEvent, JobId, Outcome, Phase, Progress, Status, Warning, WarningKind};
pub use names::{
    NameError, NameHazard, PART_SUFFIX, addressable, is_partial, windows_hazard, windows_verbatim,
};
pub use remove::{DeleteMode, SystemTrash, TrashCan};

use crate::entry::{Entry, EntryKind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

/// What the user asked for.
///
/// An enum with the operands inside each variant rather than a struct with
/// optional fields, so "a copy with no destination" and "a rename with three
/// sources" cannot be written down, let alone reach the filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    /// Copy each source into the destination *directory*, keeping its name.
    Copy {
        sources: Vec<PathBuf>,
        destination: PathBuf,
    },
    /// The same, then remove each original — but only ever after its copy is
    /// verified.
    Move {
        sources: Vec<PathBuf>,
        destination: PathBuf,
    },
    Delete {
        targets: Vec<PathBuf>,
        /// [`DeleteMode::Trash`] unless the user explicitly asked otherwise.
        mode: DeleteMode,
    },
    /// Give one entry a new name in the directory it is already in. The name is
    /// a *name*: `../elsewhere` is refused, not interpreted.
    Rename { path: PathBuf, new_name: String },
    /// `name` may be several components deep — typing `a/b/c` and getting three
    /// levels is what every orthodox file manager does — but never absolute and
    /// never containing `..`.
    MakeDirectory { parent: PathBuf, name: String },
}

/// How much re-reading is done before a destination is trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Verify {
    /// Trust the write. Only ever reasonable for a copy, where the original is
    /// still there to compare against later.
    None,
    /// The destination is as long as what was read. Catches a truncated write
    /// and a full disk; costs nothing.
    #[default]
    Size,
    /// Re-read the destination and compare a blake3 hash with the source's,
    /// which was computed while reading. Forced for every move.
    Hash,
}

/// When the data is pushed out of the page cache before the name appears.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncPolicy {
    Never,
    /// Only where a crash could lose data that no longer exists anywhere else.
    /// An fsync per file across 100,000 small files is slow enough to matter,
    /// and for a copy the source is still there.
    #[default]
    MovesOnly,
    Always,
}

/// Everything adjustable about a job.
///
/// [`Options::default`] is what F5 and F6 use. The defaults are the careful
/// ones: symlinks are not followed, moves verify by hash, deletion goes to the
/// trash.
#[derive(Debug, Clone)]
pub struct Options {
    /// Following turns a copy into an unbounded walk of the filesystem — a link
    /// to `/` makes the job never end — and a delete into a catastrophe outside
    /// the tree the user pointed at. Off unless someone means it.
    pub follow_symlinks: bool,
    pub verify: Verify,
    pub sync: SyncPolicy,
    /// Clone on APFS, Btrfs, XFS and ReFS where the filesystem allows it: a
    /// 100 GB "copy" that finishes instantly and shares extents with its
    /// source. Falls back silently, since a filesystem that cannot do it says
    /// so immediately and costs nothing.
    pub reflink: bool,
    /// Two names for one inode arrive as two names for one inode.
    pub preserve_hardlinks: bool,
    /// Move by copy-verify-delete even when a `rename` would have done.
    ///
    /// Off by default: within one filesystem a rename is atomic and instant,
    /// and no amount of verification improves on that. It exists for two real
    /// reasons — a network mount whose rename semantics you do not trust, and
    /// the fact that it is the only way to exercise the cross-device path (the
    /// dangerous one, where a source is deleted after a copy) in a test on a
    /// machine with one filesystem.
    pub always_copy_on_move: bool,
    /// The floor on how often the channel hears from a job. The counters are
    /// exact regardless; this only decides how often they are published.
    pub progress_interval: Duration,
    /// How many failures and warnings are kept in full before only being
    /// counted. A disconnected drive would otherwise produce 100,000 error
    /// strings describing one event.
    pub report_cap: usize,
    /// Overridable so tests can prove that a refusing trash deletes nothing,
    /// without putting a test's rubbish in the developer's own bin.
    pub trash: Option<Arc<dyn TrashCan>>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            follow_symlinks: false,
            verify: Verify::Size,
            sync: SyncPolicy::MovesOnly,
            reflink: true,
            preserve_hardlinks: true,
            always_copy_on_move: false,
            progress_interval: Duration::from_millis(100),
            report_cap: 1000,
            trash: None,
        }
    }
}

/// The stop flag, shared with whoever holds the handle.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// The handle on a running job.
///
/// Cloneable, and dropping it does **not** cancel: a move that abandoned itself
/// halfway because a UI struct went out of scope would be a genuinely
/// frightening bug. Stopping is always something someone asked for.
#[derive(Debug, Clone)]
pub struct JobHandle {
    id: JobId,
    cancel: CancelToken,
    standing: StandingChoice,
    running: Arc<AtomicBool>,
}

impl JobHandle {
    pub fn id(&self) -> JobId {
        self.id
    }

    /// Stop as soon as the current chunk is written. Also unblocks a job that
    /// is waiting on a conflict nobody answered.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// False once the job has published its [`Outcome`].
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// The "apply to all" currently in force, if any. The UI shows it so the
    /// user knows why they have stopped being asked.
    pub fn standing_choice(&self) -> Option<ConflictChoice> {
        self.standing.get()
    }

    /// Take back an "apply to all". The next conflict asks again.
    pub fn revoke_standing_choice(&self) {
        self.standing.revoke();
    }
}

/// Start a job on its own thread.
///
/// A dedicated thread rather than `spawn_blocking`: a copy can run for an hour,
/// and a task that long has no business sitting in a pool the rest of the
/// application shares. The channel is a Tokio one so the UI can select on it
/// alongside everything else it is waiting for.
///
/// The channel is unbounded, which is safe only because the sender coalesces —
/// see [`event`]. An unbounded channel behind an uncoalesced sender would be a
/// memory leak with a UI stall as its trigger.
pub fn spawn(
    job: Job,
    options: Options,
) -> std::io::Result<(JobHandle, UnboundedReceiver<JobEvent>)> {
    let id = JobId::next();
    let (tx, rx) = unbounded_channel();
    let cancel = CancelToken::default();
    let standing = StandingChoice::default();
    let running = Arc::new(AtomicBool::new(true));

    let handle = JobHandle {
        id,
        cancel: cancel.clone(),
        standing: standing.clone(),
        running: running.clone(),
    };

    std::thread::Builder::new()
        .name(format!("dmac-fileop-{}", id.0))
        .spawn(move || {
            let _ = engine::run(id, job, options, cancel, standing, tx);
            running.store(false, Ordering::SeqCst);
        })?;

    Ok((handle, rx))
}

/// Turn a panel's selection into the paths a [`Job`] takes.
///
/// [`Panel::operands`](crate::Panel::operands) hands back entries, which carry
/// names and nothing else. Joining those names to a location is a security
/// boundary, not a formatting step: an entry's name comes from a backend
/// listing — an archive, an SFTP server, an S3 bucket — and a listing that
/// names `../../.ssh/authorized_keys` is a file manager writing wherever the
/// server chose. Every name goes through the same one-component rule as a
/// typed rename.
pub fn sources_from(location: &Path, entries: &[&Entry]) -> Result<Vec<PathBuf>, NameError> {
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        if e.kind == EntryKind::Parent {
            continue; // `..` is never an operand; the panel already refuses it
        }
        names::validate_component(&e.name)?;
        out.push(location.join(&e.name));
    }
    Ok(out)
}

#[cfg(test)]
mod api_tests {
    use super::*;

    fn entry(name: &str) -> Entry {
        Entry {
            name: name.into(),
            kind: EntryKind::File,
            size: Some(1),
            modified: None,
            mode: None,
            selected: true,
        }
    }

    #[test]
    fn a_selection_becomes_paths_under_the_panel_location() {
        let a = entry("one.txt");
        let b = entry("two.txt");
        let got = sources_from(Path::new("/panel"), &[&a, &b]).unwrap();
        assert_eq!(
            got,
            vec![
                PathBuf::from("/panel/one.txt"),
                PathBuf::from("/panel/two.txt")
            ]
        );
    }

    /// A listing is data, and this one is hostile. Joining it blindly would
    /// have a copy write outside the directory the user is looking at.
    #[test]
    fn a_backend_that_names_a_path_traversal_is_refused() {
        let evil = entry("../../.ssh/authorized_keys");
        assert!(sources_from(Path::new("/panel"), &[&evil]).is_err());

        let dotdot = entry("..");
        let mut dotdot = dotdot;
        dotdot.kind = EntryKind::File; // not the panel's own `..` row
        assert!(sources_from(Path::new("/panel"), &[&dotdot]).is_err());
    }

    #[test]
    fn the_parent_row_never_becomes_an_operand() {
        let parent = Entry::parent();
        let f = entry("real.txt");
        let got = sources_from(Path::new("/panel"), &[&parent, &f]).unwrap();
        assert_eq!(got, vec![PathBuf::from("/panel/real.txt")]);
    }

    #[test]
    fn the_defaults_are_the_careful_ones() {
        let o = Options::default();
        assert!(!o.follow_symlinks, "following symlinks is never a default");
        assert_eq!(o.verify, Verify::Size);
        assert_eq!(DeleteMode::default(), DeleteMode::Trash);
    }

    #[test]
    fn a_handle_reports_the_job_it_belongs_to() {
        let t = CancelToken::default();
        assert!(!t.is_cancelled());
        t.cancel();
        assert!(t.is_cancelled());
    }
}
