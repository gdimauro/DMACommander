//! What happens when the destination is already there.
//!
//! The engine never decides this. It stops, describes both files, and waits.
//! That is a deliberate cost: a job that answers its own conflicts is a job
//! that overwrites something the user wanted, and no amount of "sensible
//! defaults" makes that recoverable.
//!
//! The prompt carries both sides' size, kind and modification time because the
//! dialog has to say *which* file it is about to lose. "A file with this name
//! already exists. Overwrite?" is not a question anybody can answer correctly.

use crate::entry::EntryKind;
use std::path::{Path, PathBuf};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// One end of a collision, described well enough to choose between them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Side {
    pub path: PathBuf,
    pub kind: EntryKind,
    /// `None` when the platform would not say. Rendered as `?`, never as `0` —
    /// a dialog that claims a file is empty when it could not stat it has
    /// talked somebody into deleting their work.
    pub size: Option<u64>,
    pub modified: Option<SystemTime>,
}

impl Side {
    pub(crate) fn of(path: &Path, meta: &std::fs::Metadata) -> Self {
        let kind = if meta.is_dir() {
            EntryKind::Dir
        } else if meta.is_symlink() {
            EntryKind::Symlink
        } else if meta.is_file() {
            EntryKind::File
        } else {
            EntryKind::Other
        };
        Self {
            path: path.to_path_buf(),
            kind,
            size: meta.is_file().then_some(meta.len()),
            modified: meta.modified().ok(),
        }
    }
}

/// Whether the two sides are even the same sort of thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictKind {
    /// File over file, or link over link. The ordinary case.
    SameKind,
    /// A file landing on a directory, or a directory on a file. Overwriting
    /// here means destroying something structurally different from what the
    /// user was thinking about — usually a whole tree — so a standing "yes to
    /// all" from a file-over-file question never covers it.
    KindMismatch,
}

/// A collision, with everything the dialog needs on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub source: Side,
    pub destination: Side,
    pub kind: ConflictKind,
}

impl Conflict {
    pub(crate) fn new(source: Side, destination: Side) -> Self {
        let kind = if (source.kind == EntryKind::Dir) == (destination.kind == EntryKind::Dir) {
            ConflictKind::SameKind
        } else {
            ConflictKind::KindMismatch
        };
        Self {
            source,
            destination,
            kind,
        }
    }
}

/// What the user chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictChoice {
    /// Replace the destination. For a directory landing on a file this removes
    /// the file; for a file landing on a directory it removes the whole tree.
    Overwrite,
    /// Leave both files alone. A skipped file inside a *move* also means the
    /// source directory holding it survives — nothing is removed on the basis
    /// of a copy that did not happen.
    Skip,
    /// Let the engine pick `name (2)`, `name (3)`, …
    AutoRename,
    /// A name the user typed. Never applied to more than the one conflict it
    /// answered — see [`Resolution::apply_to_all`].
    RenameTo(String),
    /// Replace only if the source is more recently modified.
    OverwriteIfNewer,
    /// Replace only if the source is bigger. The rescue-a-truncated-download
    /// case.
    OverwriteIfLarger,
    /// Stop the whole job here. Everything already finished stays finished.
    Abort,
}

impl ConflictChoice {
    /// Whether this answer can stand for conflicts the user has not seen.
    ///
    /// [`ConflictChoice::RenameTo`] cannot, and this is not a matter of taste:
    /// applying one typed name to a thousand files would have each one
    /// overwrite the last, and the job would report a thousand successes with
    /// one file to show for it. The engine drops `apply_to_all` for it rather
    /// than offering the user a way to destroy their selection.
    pub fn is_repeatable(&self) -> bool {
        !matches!(self, ConflictChoice::RenameTo(_))
    }
}

/// The answer, plus whether it stands for the rest of the job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub choice: ConflictChoice,
    /// Ignored for [`ConflictChoice::RenameTo`], and never applied to a
    /// [`ConflictKind::KindMismatch`] — those are asked one at a time, always.
    pub apply_to_all: bool,
}

impl Resolution {
    pub fn once(choice: ConflictChoice) -> Self {
        Self {
            choice,
            apply_to_all: false,
        }
    }

    pub fn all(choice: ConflictChoice) -> Self {
        Self {
            choice,
            apply_to_all: true,
        }
    }
}

/// A question the engine is blocked on.
///
/// Answer it with [`ConflictPrompt::answer`], or drop it. Dropping is safe and
/// deliberate: the engine takes it as "nobody is there", aborts, and touches
/// neither file. The one thing it will never do is guess.
#[derive(Debug)]
pub struct ConflictPrompt {
    conflict: Conflict,
    reply: SyncSender<Resolution>,
}

impl ConflictPrompt {
    pub(crate) fn new(conflict: Conflict, reply: SyncSender<Resolution>) -> Self {
        Self { conflict, reply }
    }

    pub fn conflict(&self) -> &Conflict {
        &self.conflict
    }

    /// Unblock the job. Consumes the prompt: one question, one answer.
    pub fn answer(self, resolution: Resolution) {
        // A closed receiver means the job died between asking and being
        // answered — cancelled, most likely. Nothing to report.
        let _ = self.reply.send(resolution);
    }
}

/// An "apply to all" that is still in force, shared with the [`JobHandle`] so
/// the user can take it back mid-job.
///
/// [`JobHandle`]: super::JobHandle
#[derive(Debug, Clone, Default)]
pub struct StandingChoice(Arc<Mutex<Option<ConflictChoice>>>);

impl StandingChoice {
    /// What is standing, if anything.
    ///
    /// A poisoned lock reads as "nothing" — the failure direction is one more
    /// question, never one more silent overwrite.
    pub fn get(&self) -> Option<ConflictChoice> {
        self.0.lock().ok().and_then(|g| g.clone())
    }

    pub(crate) fn set(&self, choice: ConflictChoice) {
        if let Ok(mut g) = self.0.lock() {
            *g = Some(choice);
        }
    }

    /// Take back the "apply to all". The next conflict asks again.
    pub fn revoke(&self) {
        if let Ok(mut g) = self.0.lock() {
            *g = None;
        }
    }
}

/// What a choice means for this particular pair of files, once the comparisons
/// have been made. Pure, so the "only if newer" rules are testable without a
/// filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Decision {
    Overwrite,
    Skip,
    /// The comparison the user asked for cannot be made — a timestamp the
    /// platform would not give us. Skipped and said out loud; guessing "newer"
    /// from no evidence is how a good file gets replaced by a stale one.
    Undecidable(&'static str),
    AutoRename,
    RenameTo(String),
    Abort,
}

pub(crate) fn decide(choice: &ConflictChoice, c: &Conflict) -> Decision {
    match choice {
        ConflictChoice::Overwrite => Decision::Overwrite,
        ConflictChoice::Skip => Decision::Skip,
        ConflictChoice::AutoRename => Decision::AutoRename,
        ConflictChoice::RenameTo(n) => Decision::RenameTo(n.clone()),
        ConflictChoice::Abort => Decision::Abort,
        ConflictChoice::OverwriteIfNewer => {
            match (c.source.modified, c.destination.modified) {
                (Some(s), Some(d)) if s > d => Decision::Overwrite,
                (Some(_), Some(_)) => Decision::Skip,
                // Equal timestamps are not "newer". Copying anyway would churn
                // a backup target for no gain.
                _ => Decision::Undecidable("one of the two has no modification time"),
            }
        }
        ConflictChoice::OverwriteIfLarger => match (c.source.size, c.destination.size) {
            (Some(s), Some(d)) if s > d => Decision::Overwrite,
            (Some(_), Some(_)) => Decision::Skip,
            _ => Decision::Undecidable("one of the two has no size (it is not a plain file)"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn side(name: &str, kind: EntryKind, size: Option<u64>, secs: Option<u64>) -> Side {
        Side {
            path: PathBuf::from(name),
            kind,
            size,
            modified: secs.map(|s| SystemTime::UNIX_EPOCH + Duration::from_secs(s)),
        }
    }

    fn file_pair(src: (u64, u64), dst: (u64, u64)) -> Conflict {
        Conflict::new(
            side("/a/x", EntryKind::File, Some(src.0), Some(src.1)),
            side("/b/x", EntryKind::File, Some(dst.0), Some(dst.1)),
        )
    }

    #[test]
    fn file_over_file_is_the_ordinary_case() {
        assert_eq!(file_pair((1, 1), (1, 1)).kind, ConflictKind::SameKind);
    }

    /// The dangerous shape: saying "overwrite" about a text file must never be
    /// stretched into deleting a directory tree.
    #[test]
    fn a_file_landing_on_a_directory_is_a_different_kind_of_question() {
        let c = Conflict::new(
            side("/a/x", EntryKind::File, Some(10), None),
            side("/b/x", EntryKind::Dir, None, None),
        );
        assert_eq!(c.kind, ConflictKind::KindMismatch);
    }

    #[test]
    fn newer_only_needs_both_timestamps() {
        let c = file_pair((10, 200), (10, 100));
        assert_eq!(
            decide(&ConflictChoice::OverwriteIfNewer, &c),
            Decision::Overwrite
        );
        let c = file_pair((10, 100), (10, 200));
        assert_eq!(
            decide(&ConflictChoice::OverwriteIfNewer, &c),
            Decision::Skip
        );
        let c = file_pair((10, 100), (10, 100));
        assert_eq!(
            decide(&ConflictChoice::OverwriteIfNewer, &c),
            Decision::Skip,
            "the same second is not newer"
        );
    }

    /// No timestamp is not "older". Guessing here replaces a good file with a
    /// stale one and reports success.
    #[test]
    fn newer_only_refuses_to_guess_without_a_timestamp() {
        let c = Conflict::new(
            side("/a/x", EntryKind::File, Some(10), None),
            side("/b/x", EntryKind::File, Some(10), Some(100)),
        );
        assert!(matches!(
            decide(&ConflictChoice::OverwriteIfNewer, &c),
            Decision::Undecidable(_)
        ));
    }

    #[test]
    fn larger_only_compares_sizes() {
        assert_eq!(
            decide(
                &ConflictChoice::OverwriteIfLarger,
                &file_pair((99, 0), (10, 0))
            ),
            Decision::Overwrite
        );
        assert_eq!(
            decide(
                &ConflictChoice::OverwriteIfLarger,
                &file_pair((10, 0), (99, 0))
            ),
            Decision::Skip
        );
    }

    /// One typed name applied to a thousand files means a thousand overwrites
    /// of the same destination.
    #[test]
    fn a_typed_name_can_never_be_applied_to_all() {
        assert!(!ConflictChoice::RenameTo("x".into()).is_repeatable());
        assert!(ConflictChoice::Overwrite.is_repeatable());
        assert!(ConflictChoice::Skip.is_repeatable());
        assert!(ConflictChoice::AutoRename.is_repeatable());
    }

    #[test]
    fn a_standing_choice_can_be_taken_back() {
        let s = StandingChoice::default();
        assert_eq!(s.get(), None);
        s.set(ConflictChoice::Overwrite);
        assert_eq!(s.get(), Some(ConflictChoice::Overwrite));
        s.revoke();
        assert_eq!(s.get(), None, "the user must be able to change their mind");
    }

    #[test]
    fn a_dropped_prompt_tells_the_engine_nobody_answered() {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let prompt = ConflictPrompt::new(file_pair((1, 1), (1, 1)), tx);
        drop(prompt);
        assert!(rx.recv().is_err());
    }
}
