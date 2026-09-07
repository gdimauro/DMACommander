//! The job runtime: scan, then transfer, with a question in the middle when
//! the destination is already occupied.
//!
//! Everything in here runs on the job's own thread and touches no shared state
//! except three things it is given: a cancel flag, the standing conflict
//! choice, and the event channel. That is what makes it safe to run several
//! jobs at once, and what keeps the render loop free — the UI's only
//! involvement is draining a channel and, occasionally, answering a question.
//!
//! Two passes over the tree, deliberately. The first counts; the second moves
//! bytes. It costs a second `read_dir` walk and buys the only honest progress
//! bar there is: without a denominator the UI has nothing to draw but a
//! spinner, and a spinner for a four-hour copy tells the user nothing about
//! whether to go to lunch.
//!
//! Both walks use an explicit stack rather than recursion. Not for speed: an
//! extracted archive can be thousands of levels deep, and a stack overflow in
//! the middle of a *move* would abandon the tree with no report of what had
//! already been deleted.

use super::conflict::{
    Conflict, ConflictChoice, ConflictKind, ConflictPrompt, Decision, Side, StandingChoice, decide,
};
use super::error::{FailureKind, FileOpsError, Result};
use super::event::{JobEvent, JobId, Phase, Reporter, Status, WarningKind};
use super::file::{self, CopyReport};
use super::meta;
use super::names;
use super::remove::{self, DeleteMode, SystemTrash, TrashCan};
use super::{CancelToken, Job, Options, Outcome, SyncPolicy, Verify};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How often the conflict wait wakes up to notice a cancellation.
///
/// Without it the job would sit in `recv` forever and Esc would do nothing
/// while a dialog was open. A job that cannot be cancelled "at any instant" is
/// one the user learns to be afraid of starting.
const CONFLICT_POLL: Duration = Duration::from_millis(100);

/// How many times a renamed destination may itself collide before the engine
/// gives up asking, so a dialog loop can never become inescapable.
const MAX_REASKS: u32 = 8;

/// Everything one job needs, on one thread, with no locks.
pub(crate) struct Ctx {
    pub(crate) opts: Options,
    pub(crate) reporter: Reporter,
    cancel: CancelToken,
    standing: StandingChoice,
    /// Reused across every file, so a 100k-file job allocates one buffer.
    pub(crate) buf: Vec<u8>,
    /// inode -> the first destination we wrote for it, so the second name for
    /// one file becomes a link rather than a second copy.
    links: HashMap<(u64, u64), PathBuf>,
    /// Directories already entered. Only populated when symlinks are followed,
    /// which is the only way a "tree" can stop being one.
    seen_dirs: HashSet<(u64, u64)>,
    /// Set when a failure means every remaining entry would fail the same way.
    stop: Option<String>,
    fallback_trash: SystemTrash,
}

impl Ctx {
    pub(crate) fn cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    pub(crate) fn verify(&self, strict: bool) -> Verify {
        required_verify(self.opts.verify, strict)
    }

    /// Whether the cheap rename may be tried at all.
    fn may_rename(&self) -> bool {
        !self.opts.always_copy_on_move
    }

    pub(crate) fn sync_wanted(&self, strict: bool) -> bool {
        match self.opts.sync {
            SyncPolicy::Always => true,
            SyncPolicy::MovesOnly => strict,
            SyncPolicy::Never => false,
        }
    }

    /// Borrow the shared copy buffer for the length of one file.
    ///
    /// Handed out and handed back rather than borrowed in place: the copy loop
    /// needs the buffer and the progress counters at the same instant, and both
    /// live behind this one `&mut`.
    pub(crate) fn take_buffer(&mut self, size: usize) -> Vec<u8> {
        let buf = std::mem::take(&mut self.buf);
        if buf.len() < size {
            vec![0u8; size]
        } else {
            buf
        }
    }

    pub(crate) fn trash(&self) -> &dyn TrashCan {
        match self.opts.trash.as_deref() {
            Some(t) => t,
            None => &self.fallback_trash,
        }
    }

    /// Record a per-entry failure and keep going — unless it is the kind of
    /// failure that will repeat for every remaining entry.
    pub(crate) fn fail_io(&mut self, path: &Path, action: &str, e: std::io::Error) {
        let kind = FailureKind::of(&e);
        let message = format!("{action} failed: {e}");
        if kind.is_terminal() {
            self.stop = Some(message.clone());
        }
        self.reporter.fail(path.to_path_buf(), kind, message);
    }

    fn fail(&mut self, path: &Path, e: FileOpsError) {
        let kind = match &e {
            FileOpsError::Io { source, .. } => FailureKind::of(source),
            FileOpsError::Name(_) => FailureKind::IllegalName,
            _ => FailureKind::Other,
        };
        if kind.is_terminal() {
            self.stop = Some(e.to_string());
        }
        self.reporter.fail(path.to_path_buf(), kind, e.to_string());
    }

    fn note(&mut self, path: &Path, problems: Vec<meta::MetaProblem>) {
        for (kind, detail) in problems {
            self.reporter.warn(path.to_path_buf(), kind, detail);
        }
    }

    /// Put a question on the channel and wait, watching for cancellation.
    ///
    /// Exactly three things come back: a choice, "cancelled", or "nobody
    /// answered". The last is not a licence to guess — an engine that
    /// overwrites because no one was listening is the bug this whole module
    /// exists to make impossible.
    fn ask(&mut self, conflict: Conflict) -> Result<ConflictChoice> {
        // A standing "apply to all" never covers a kind mismatch: saying
        // "overwrite" about a text file must not, three thousand entries later,
        // delete a directory tree unasked.
        if conflict.kind == ConflictKind::SameKind
            && let Some(choice) = self.standing.get()
        {
            return Ok(choice);
        }
        let path = conflict.destination.path.clone();
        if !self.reporter.listening() {
            return Err(FileOpsError::Unanswered(path));
        }

        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let kind = conflict.kind;
        self.reporter
            .emit(JobEvent::Conflict(ConflictPrompt::new(conflict, tx)));

        let resolution = loop {
            match rx.recv_timeout(CONFLICT_POLL) {
                Ok(r) => break r,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if self.cancelled() {
                        return Err(FileOpsError::Cancelled);
                    }
                }
                // The prompt was dropped: the dialog closed without an answer,
                // or the UI is gone.
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(FileOpsError::Unanswered(path));
                }
            }
        };

        if resolution.apply_to_all
            && resolution.choice.is_repeatable()
            && kind == ConflictKind::SameKind
        {
            self.standing.set(resolution.choice.clone());
        }
        Ok(resolution.choice)
    }
}

/// A move never verifies less than a hash, whatever the options say.
///
/// The one setting a caller is not allowed to weaken, because the step after it
/// deletes the original. "Fast move" is not a feature worth offering when its
/// failure mode is a lost file.
pub(super) fn required_verify(configured: Verify, moving: bool) -> Verify {
    if moving { Verify::Hash } else { configured }
}

/// What the scan learned.
///
/// `bytes` counts each inode once, so a tree full of hardlinks does not promise
/// more data than will actually move — and the bar therefore reaches the end
/// instead of stopping at 30%.
#[derive(Debug, Default)]
struct Scan {
    files: u64,
    bytes: u64,
    /// Per-directory subtree totals, so a whole tree moved by a single `rename`
    /// still advances the bar by what it contained. Only built for moves, where
    /// that fast path exists.
    subtrees: HashMap<PathBuf, (u64, u64)>,
}

enum ScanStep {
    Enter(PathBuf),
    /// The counters as they were when this directory was opened. Everything
    /// between it and here belongs to this directory, because a stack walk
    /// finishes a subtree before it touches the next sibling.
    Close(PathBuf, u64, u64),
}

/// Count what is about to move.
fn scan(ctx: &mut Ctx, sources: &[PathBuf], want_subtrees: bool) -> Scan {
    let mut out = Scan::default();
    let mut seen_inodes: HashSet<(u64, u64)> = HashSet::new();
    let mut visited_dirs: HashSet<(u64, u64)> = HashSet::new();

    for source in sources {
        let mut stack = vec![ScanStep::Enter(source.clone())];
        while let Some(step) = stack.pop() {
            if ctx.cancelled() {
                return out;
            }
            match step {
                ScanStep::Close(path, files, bytes) => {
                    if want_subtrees {
                        out.subtrees
                            .insert(path, (out.files - files, out.bytes - bytes));
                    }
                }
                ScanStep::Enter(path) => {
                    let Some(meta) = stat(ctx, &path) else {
                        continue;
                    };
                    ctx.reporter.current(&path);
                    let follows = ctx.opts.follow_symlinks;
                    let dir = meta.is_dir() || (follows && meta.is_symlink() && path.is_dir());

                    if dir {
                        if follows
                            && let Some(key) = identity(&path)
                            && !visited_dirs.insert(key)
                        {
                            // Been here already: a symlink points back up the
                            // tree. Counting it again would never terminate.
                            continue;
                        }
                        stack.push(ScanStep::Close(path.clone(), out.files, out.bytes));
                        match std::fs::read_dir(&path) {
                            Ok(rd) => {
                                for e in rd.flatten() {
                                    stack.push(ScanStep::Enter(e.path()));
                                }
                            }
                            Err(e) => ctx.fail_io(&path, "list", e),
                        }
                    } else if meta.is_file() {
                        let fresh = match (ctx.opts.preserve_hardlinks, meta::hardlink_key(&meta)) {
                            (true, Some(key)) => seen_inodes.insert(key),
                            _ => true,
                        };
                        out.files += 1;
                        if fresh {
                            out.bytes += meta.len();
                        }
                    } else {
                        // A symlink, or something exotic: one entry, no bytes.
                        out.files += 1;
                    }
                }
            }
        }
    }
    out
}

/// `symlink_metadata`, recording a failure rather than stopping the job.
///
/// Permission denied on entry 50,000 of 100,000 costs one entry, not the other
/// 50,000.
fn stat(ctx: &mut Ctx, path: &Path) -> Option<std::fs::Metadata> {
    match path.symlink_metadata() {
        Ok(m) => Some(m),
        Err(e) => {
            ctx.fail_io(path, "read", e);
            None
        }
    }
}

/// The identity of a directory, for detecting a walk that has looped.
#[cfg(unix)]
fn identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    // Following, deliberately: the question is which directory this *resolves*
    // to, not which link points at it.
    path.metadata().ok().map(|m| (m.dev(), m.ino()))
}

/// No inode numbers without `windows-sys`, so loop detection there is left to
/// the filesystem's own path-length limit. Said out loud rather than pretended.
#[cfg(not(unix))]
fn identity(_path: &Path) -> Option<(u64, u64)> {
    None
}

/// One item of work in the transfer.
enum Work {
    /// Deal with `src`, which is to become `dst`.
    Visit { src: PathBuf, dst: PathBuf },
    /// Everything inside this directory has been dealt with: stamp its metadata
    /// (writing children changed its mtime) and, for a move, take the original
    /// away — but only if it came out empty.
    Close { src: PathBuf, dst: PathBuf },
}

/// The transfer walk, shared by copy and move.
///
/// `moving` changes three things and nothing else: a rename is tried first,
/// verification is forced up to a hash, and the source is removed once the
/// destination is confirmed. In that order, every time.
fn transfer(ctx: &mut Ctx, sources: &[PathBuf], destination: &Path, moving: bool, scanned: &Scan) {
    let mut stack: Vec<Work> = Vec::new();
    // Reversed, so the first thing the user selected is the first thing that
    // moves. A progress line jumping about in an order nobody chose reads as a
    // bug even when it is not.
    for src in sources.iter().rev() {
        let Some(name) = src.file_name() else {
            ctx.fail(src, FileOpsError::RootPath(src.clone()));
            continue;
        };
        stack.push(Work::Visit {
            src: src.clone(),
            dst: destination.join(name),
        });
    }

    while let Some(work) = stack.pop() {
        if ctx.cancelled() || ctx.stop.is_some() {
            return;
        }
        match work {
            Work::Visit { src, dst } => {
                if let Err(e) = visit(ctx, &src, &dst, moving, scanned, &mut stack) {
                    match e {
                        // Anything that ends the whole job, rather than one
                        // entry, stops the walk here and now. What is done is
                        // done and is complete; what is not has not started.
                        FileOpsError::Cancelled
                        | FileOpsError::AbortedAtConflict(_)
                        | FileOpsError::Unanswered(_) => {
                            ctx.stop = Some(e.to_string());
                            return;
                        }
                        other => ctx.fail(&src, other),
                    }
                }
            }
            Work::Close { src, dst } => {
                if let Ok(m) = src.symlink_metadata() {
                    let problems = meta::apply(&src, &m, &dst);
                    ctx.note(&dst, problems);
                }
                if moving {
                    remove::remove_moved_directory(ctx, &src);
                }
            }
        }
    }
}

/// Deal with one entry, pushing children if it is a directory.
fn visit(
    ctx: &mut Ctx,
    src: &Path,
    dst: &Path,
    moving: bool,
    scanned: &Scan,
    stack: &mut Vec<Work>,
) -> Result<()> {
    let Some(meta) = stat(ctx, src) else {
        return Ok(());
    };
    ctx.reporter.current(src);

    if !check_name(ctx, dst) {
        return Ok(());
    }

    let Some(dst) = resolve_destination(ctx, src, &meta, dst)? else {
        ctx.reporter.skip(1);
        return Ok(());
    };

    let follows = ctx.opts.follow_symlinks;
    let treat_as_dir = meta.is_dir() || (follows && meta.is_symlink() && src.is_dir());

    if treat_as_dir {
        return visit_directory(ctx, src, &dst, moving, scanned, stack);
    }

    if meta.is_symlink() && !follows {
        if moving && ctx.may_rename() && rename_step(src, &dst)? {
            ctx.reporter.add_files(1);
            return Ok(());
        }
        let problems = file::copy_symlink(src, &dst, &meta)?;
        ctx.note(&dst, problems);
        ctx.reporter.add_files(1);
        if moving {
            remove_source_file(ctx, src);
        }
        return Ok(());
    }

    // Resolve through the link when following, so the bytes copied are the
    // target's.
    let meta = if meta.is_symlink() {
        src.metadata()
            .map_err(|e| FileOpsError::io(src, "resolve the link", e))?
    } else {
        meta
    };

    if !meta.is_file() {
        // A fifo, a socket, a device node. Opening a fifo for reading blocks
        // until somebody writes to it, which would hang the job for ever — so
        // this is refused rather than attempted. Within one filesystem a move
        // is still a rename, and still works.
        if moving && ctx.may_rename() && rename_step(src, &dst)? {
            ctx.reporter.add_files(1);
            return Ok(());
        }
        ctx.reporter.warn(
            src.to_path_buf(),
            WarningKind::MetadataNotPreserved,
            "skipped: not a regular file (a socket, fifo or device node cannot be copied)".into(),
        );
        ctx.reporter.skip(1);
        return Ok(());
    }

    // A second name for a file already copied in this job becomes a link, so a
    // hardlinked tree does not arrive four times its size with the sharing
    // silently gone.
    if ctx.opts.preserve_hardlinks
        && let Some(key) = meta::hardlink_key(&meta)
    {
        if let Some(first) = ctx.links.get(&key).cloned() {
            match std::fs::hard_link(&first, &dst) {
                Ok(()) => {
                    ctx.reporter.add_files(1);
                    if moving {
                        remove_source_file(ctx, src);
                    }
                    return Ok(());
                }
                Err(e) => ctx.reporter.warn(
                    dst.clone(),
                    WarningKind::HardlinkNotPreserved,
                    format!("copied as an independent file: {e}"),
                ),
            }
        } else {
            ctx.links.insert(key, dst.clone());
        }
    }

    if moving && ctx.may_rename() && rename_step(src, &dst)? {
        ctx.reporter.add_files(1);
        ctx.reporter.add_bytes(meta.len());
        return Ok(());
    }

    let CopyReport {
        changed, problems, ..
    } = file::copy_file(ctx, src, &meta, &dst, moving)?;
    ctx.note(&dst, problems);
    if changed {
        ctx.reporter.warn(
            src.to_path_buf(),
            WarningKind::MetadataNotPreserved,
            "the file changed while it was being copied; what was written is the file as it was when the copy started".into(),
        );
    }
    ctx.reporter.add_files(1);

    if moving {
        // Only now. The destination is written, flushed, hash-verified and
        // renamed into place; this is the first moment at which removing the
        // original is not a gamble.
        remove_source_file(ctx, src);
    }
    Ok(())
}

fn visit_directory(
    ctx: &mut Ctx,
    src: &Path,
    dst: &Path,
    moving: bool,
    scanned: &Scan,
    stack: &mut Vec<Work>,
) -> Result<()> {
    // A whole subtree that can simply be renamed is the best outcome there is:
    // instant, atomic, and the data is never in two places or in none.
    if moving && ctx.may_rename() && dst.symlink_metadata().is_err() && rename_step(src, dst)? {
        let (files, bytes) = scanned.subtrees.get(src).copied().unwrap_or((0, 0));
        ctx.reporter.add_files(files);
        ctx.reporter.add_bytes(bytes);
        return Ok(());
    }

    if ctx.opts.follow_symlinks
        && let Some(key) = identity(src)
        && !ctx.seen_dirs.insert(key)
    {
        ctx.reporter.warn(
            src.to_path_buf(),
            WarningKind::SourceKept,
            "skipped: following symlinks leads back into a directory already copied".into(),
        );
        ctx.reporter.skip(1);
        return Ok(());
    }

    create_dir(dst).map_err(|e| FileOpsError::io(dst, "create the directory", e))?;
    stack.push(Work::Close {
        src: src.to_path_buf(),
        dst: dst.to_path_buf(),
    });
    match std::fs::read_dir(src) {
        Ok(rd) => {
            for e in rd.flatten() {
                stack.push(Work::Visit {
                    src: e.path(),
                    dst: dst.join(e.file_name()),
                });
            }
        }
        Err(e) => ctx.fail_io(src, "list", e),
    }
    Ok(())
}

/// Warn about — or on Windows, refuse — a name the destination cannot hold.
///
/// Returns `false` when the entry was skipped.
fn check_name(ctx: &mut Ctx, dst: &Path) -> bool {
    let Some(name) = dst.file_name().and_then(|n| n.to_str()) else {
        return true;
    };
    let Some(hazard) = names::windows_hazard(name) else {
        return true;
    };
    if cfg!(windows) {
        ctx.reporter.fail(
            dst.to_path_buf(),
            FailureKind::IllegalName,
            format!("skipped: the name is {hazard}"),
        );
        ctx.reporter.skip(1);
        return false;
    }
    // On Unix these are legal names and copying them locally is correct. The
    // warning is for the tree that is on its way to a Windows share, where
    // `report.` and `report` become one file and the second silently wins.
    ctx.reporter.warn(
        dst.to_path_buf(),
        WarningKind::NameHazard,
        format!("the name is {hazard}; it will not survive a copy to Windows"),
    );
    true
}

/// Try the cheap move. `false` means "different filesystem, do it the long
/// way"; a real error is returned as one, because falling back to copy-then-
/// delete after a rename we were not allowed to do usually ends in a copy we
/// cannot delete the source of.
fn rename_step(src: &Path, dst: &Path) -> Result<bool> {
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(true),
        Err(e) if crosses_devices(&e) => Ok(false),
        Err(e) => Err(FileOpsError::io(src, "move", e)),
    }
}

/// Whether a failed rename means "different filesystem".
///
/// The `ErrorKind` covers it on current platforms; the raw code is checked as
/// well because getting this wrong is expensive in both directions — a real
/// failure misread as cross-device becomes a slow copy of something that could
/// not be renamed for an entirely different reason, and a cross-device rename
/// misread as a failure breaks every move between two disks.
fn crosses_devices(e: &std::io::Error) -> bool {
    if e.kind() == std::io::ErrorKind::CrossesDevices {
        return true;
    }
    #[cfg(unix)]
    {
        e.raw_os_error() == Some(18) // EXDEV
    }
    #[cfg(windows)]
    {
        e.raw_os_error() == Some(17) // ERROR_NOT_SAME_DEVICE
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

fn create_dir(dst: &Path) -> std::io::Result<()> {
    match std::fs::create_dir(dst) {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && dst.is_dir() => Ok(()),
        other => other,
    }
}

/// Delete one source file, after its copy has been verified.
fn remove_source_file(ctx: &mut Ctx, src: &Path) {
    if remove::check_removable(src).is_err() {
        return;
    }
    if let Err(e) = std::fs::remove_file(src) {
        ctx.fail_io(src, "remove the original after copying it", e);
    }
}

/// Decide where this entry actually goes, asking if something is in the way.
///
/// `Ok(None)` means skip. The path that comes back may not be the one that went
/// in (the user renamed), and the destination may have been removed on the way
/// (the user chose to replace a directory) — but a plain file is **never**
/// removed here. `rename` replaces one atomically, and a delete-then-write
/// window is a window in which the user has neither file.
fn resolve_destination(
    ctx: &mut Ctx,
    src: &Path,
    src_meta: &std::fs::Metadata,
    dst: &Path,
) -> Result<Option<PathBuf>> {
    let mut dst = dst.to_path_buf();

    for _ in 0..MAX_REASKS {
        let Ok(dst_meta) = dst.symlink_metadata() else {
            return Ok(Some(dst)); // nothing there, nothing to ask
        };

        // The same file under two names. Copying it onto itself truncates the
        // source to nothing and reports success.
        if meta::same_file(src, &dst) {
            return Err(FileOpsError::SameFile(dst));
        }

        // Two directories merge. Nobody wants this question asked: the
        // interesting conflicts are the files inside, and offering "overwrite"
        // for a directory invites deleting a tree the user meant to merge into.
        if src_meta.is_dir() && dst_meta.is_dir() {
            return Ok(Some(dst));
        }

        let conflict = Conflict::new(Side::of(src, src_meta), Side::of(&dst, &dst_meta));
        let choice = ctx.ask(conflict.clone())?;
        match decide(&choice, &conflict) {
            Decision::Skip => return Ok(None),
            Decision::Undecidable(why) => {
                ctx.reporter.warn(
                    dst.clone(),
                    WarningKind::MetadataNotPreserved,
                    format!("skipped: {why}, so the comparison you asked for could not be made"),
                );
                return Ok(None);
            }
            Decision::Abort => return Err(FileOpsError::AbortedAtConflict(dst)),
            Decision::Overwrite => {
                if dst_meta.is_dir() {
                    // A directory cannot be replaced by a rename, so it has to
                    // go first. This is the most destructive thing the engine
                    // ever does, and it happens only on an explicit per-item
                    // answer: a standing choice never reaches this line,
                    // because a kind mismatch is always asked individually.
                    remove::remove_tree(ctx, &dst)?;
                }
                return Ok(Some(dst));
            }
            Decision::AutoRename => {
                let Some(next) = names::unique_name(&dst, |p| p.symlink_metadata().is_ok()) else {
                    return Err(FileOpsError::io(
                        &dst,
                        "find a free name",
                        std::io::Error::other("every candidate name is taken"),
                    ));
                };
                return Ok(Some(next));
            }
            Decision::RenameTo(name) => {
                names::validate_component(&name)?;
                let Some(parent) = dst.parent() else {
                    return Err(FileOpsError::RootPath(dst));
                };
                // Round again: the name the user typed may itself be taken, and
                // silently overwriting *that* would be the same bug wearing a
                // different hat.
                dst = parent.join(name);
            }
        }
    }
    Err(FileOpsError::io(
        &dst,
        "resolve the conflict",
        std::io::Error::other("too many renames in a row"),
    ))
}

/// Refuse the shapes that cannot end well, before anything is touched.
fn check_transfer(sources: &[PathBuf], destination: &Path) -> Result<()> {
    if sources.is_empty() {
        return Err(FileOpsError::NoSources);
    }
    if !destination.is_dir() {
        return Err(FileOpsError::NotADirectory(destination.to_path_buf()));
    }
    // Canonical form on both sides: `/tmp` is a symlink to `/private/tmp` on
    // macOS, and comparing the two spellings would miss the very case this
    // check exists for.
    let dst_real = destination
        .canonicalize()
        .map_err(|e| FileOpsError::io(destination, "resolve", e))?;
    for src in sources {
        let Ok(src_real) = src.canonicalize() else {
            continue; // a source that is not there fails per-entry, later
        };
        if src_real == dst_real {
            return Err(FileOpsError::SameFile(src.clone()));
        }
        if src.is_dir() && dst_real.starts_with(&src_real) {
            return Err(FileOpsError::DestinationInsideSource {
                path: destination.to_path_buf(),
                source_path: src.clone(),
            });
        }
    }
    Ok(())
}

/// Run a job to completion on the calling thread.
///
/// Crate-private on purpose: everything outside goes through
/// [`super::spawn`], because a file job on the UI thread is the definition of a
/// dropped frame.
pub(crate) fn run(
    id: JobId,
    job: Job,
    opts: Options,
    cancel: CancelToken,
    standing: StandingChoice,
    tx: tokio::sync::mpsc::UnboundedSender<JobEvent>,
) -> Outcome {
    let reporter = Reporter::new(tx, opts.progress_interval, opts.report_cap);
    let mut ctx = Ctx {
        reporter,
        cancel,
        standing,
        buf: Vec::new(),
        links: HashMap::new(),
        seen_dirs: HashSet::new(),
        stop: None,
        fallback_trash: SystemTrash,
        opts,
    };
    ctx.reporter.emit(JobEvent::Started(id));

    let result = dispatch(&mut ctx, job);

    // Cancellation first: a cancel that arrives while a walk is mid-directory
    // surfaces as an ordinary early return, and reporting it as an abort would
    // tell the user something went wrong when they are the one who stopped it.
    let status = if ctx.cancelled() {
        Status::Cancelled
    } else {
        match result {
            Err(e) => Status::Aborted(e.to_string()),
            Ok(()) => match ctx.stop.take() {
                Some(why) => Status::Aborted(why),
                None if ctx.reporter.has_failures() => Status::CompletedWithFailures,
                None => Status::Completed,
            },
        }
    };
    ctx.reporter.finish(id, status)
}

fn dispatch(ctx: &mut Ctx, job: Job) -> Result<()> {
    // One place where every path the job will touch is put into the form this
    // platform can address. Everything deeper is built by joining onto these,
    // so a Windows tree deeper than 260 characters works from here down.
    match addressable(job) {
        Job::Copy {
            sources,
            destination,
        } => {
            check_transfer(&sources, &destination)?;
            let scanned = scan(ctx, &sources, false);
            ctx.reporter.set_totals(scanned.files, scanned.bytes);
            ctx.reporter.phase(Phase::Transferring);
            transfer(ctx, &sources, &destination, false, &scanned);
            Ok(())
        }
        Job::Move {
            sources,
            destination,
        } => {
            check_transfer(&sources, &destination)?;
            let scanned = scan(ctx, &sources, true);
            ctx.reporter.set_totals(scanned.files, scanned.bytes);
            ctx.reporter.phase(Phase::Transferring);
            transfer(ctx, &sources, &destination, true, &scanned);
            Ok(())
        }
        Job::Delete { targets, mode } => delete(ctx, targets, mode),
        Job::Rename { path, new_name } => rename(ctx, path, new_name),
        Job::MakeDirectory { parent, name } => make_directory(ctx, parent, name),
    }
}

/// Put every path in a job into the form the platform can open. A no-op
/// everywhere but Windows, and there only for paths long enough to need it.
fn addressable(job: Job) -> Job {
    let all = |v: Vec<PathBuf>| v.iter().map(|p| names::addressable(p)).collect();
    match job {
        Job::Copy {
            sources,
            destination,
        } => Job::Copy {
            sources: all(sources),
            destination: names::addressable(&destination),
        },
        Job::Move {
            sources,
            destination,
        } => Job::Move {
            sources: all(sources),
            destination: names::addressable(&destination),
        },
        Job::Delete { targets, mode } => Job::Delete {
            targets: all(targets),
            mode,
        },
        Job::Rename { path, new_name } => Job::Rename {
            path: names::addressable(&path),
            new_name,
        },
        Job::MakeDirectory { parent, name } => Job::MakeDirectory {
            parent: names::addressable(&parent),
            name,
        },
    }
}

fn delete(ctx: &mut Ctx, targets: Vec<PathBuf>, mode: DeleteMode) -> Result<()> {
    if targets.is_empty() {
        return Err(FileOpsError::NoSources);
    }
    // Every target is checked before the first one is touched: a root path in
    // the list is a bug upstream, and finding it halfway through is finding it
    // too late.
    for t in &targets {
        remove::check_removable(t)?;
    }

    match mode {
        // The trash takes a whole tree in one move, so the unit of work is the
        // target, not the file — and there is no reason to walk 100,000 entries
        // to produce a number that will never be counted up to.
        DeleteMode::Trash => ctx.reporter.set_totals(targets.len() as u64, 0),
        DeleteMode::Permanent => {
            let scanned = scan(ctx, &targets, false);
            ctx.reporter.set_totals(scanned.files, scanned.bytes);
        }
    }
    ctx.reporter.phase(Phase::Deleting);

    for target in &targets {
        if ctx.cancelled() || ctx.stop.is_some() {
            break;
        }
        let result = match mode {
            DeleteMode::Trash => remove::to_trash(ctx, target),
            DeleteMode::Permanent => remove::remove_tree(ctx, target),
        };
        match result {
            Ok(()) => {
                if mode == DeleteMode::Trash {
                    ctx.reporter.add_files(1);
                }
            }
            Err(FileOpsError::Cancelled) => return Ok(()),
            Err(other) => ctx.fail(target, other),
        }
    }
    Ok(())
}

fn rename(ctx: &mut Ctx, path: PathBuf, new_name: String) -> Result<()> {
    names::validate_component(&new_name)?;
    remove::check_removable(&path)?;
    let Some(parent) = path.parent() else {
        return Err(FileOpsError::RootPath(path));
    };
    let dst = parent.join(&new_name);
    let meta = path
        .symlink_metadata()
        .map_err(|e| FileOpsError::io(&path, "read", e))?;

    ctx.reporter.set_totals(1, 0);
    ctx.reporter.phase(Phase::Transferring);
    ctx.reporter.current(&path);

    // On a case-insensitive filesystem `notes.txt` -> `Notes.txt` finds its own
    // destination "already there". Treating that as a conflict and honouring
    // "overwrite" would delete the file and then rename nothing — so the
    // identity check comes first, before anyone is asked anything.
    let dst = if meta::same_file(&path, &dst) {
        dst
    } else {
        match resolve_destination(ctx, &path, &meta, &dst)? {
            Some(d) => d,
            None => {
                ctx.reporter.skip(1);
                return Ok(());
            }
        }
    };

    std::fs::rename(&path, &dst).map_err(|e| FileOpsError::io(&path, "rename", e))?;
    ctx.reporter.add_files(1);
    Ok(())
}

fn make_directory(ctx: &mut Ctx, parent: PathBuf, name: String) -> Result<()> {
    names::validate_relative(&name)?;
    if !parent.is_dir() {
        return Err(FileOpsError::NotADirectory(parent));
    }
    let path = parent.join(&name);
    if path.symlink_metadata().is_ok() {
        return Err(FileOpsError::io(
            &path,
            "create the directory",
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "something with that name is already there",
            ),
        ));
    }
    ctx.reporter.set_totals(1, 0);
    std::fs::create_dir_all(&path)
        .map_err(|e| FileOpsError::io(&path, "create the directory", e))?;
    ctx.reporter.add_files(1);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn a_copy_into_its_own_subtree_is_refused_before_anything_moves() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("tree");
        let inner = src.join("inner");
        std::fs::create_dir_all(&inner).unwrap();
        let err = check_transfer(std::slice::from_ref(&src), &inner).unwrap_err();
        assert!(
            matches!(err, FileOpsError::DestinationInsideSource { .. }),
            "{err}"
        );
    }

    #[test]
    fn copying_a_directory_onto_itself_is_refused() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("tree");
        std::fs::create_dir(&src).unwrap();
        assert!(matches!(
            check_transfer(std::slice::from_ref(&src), &src).unwrap_err(),
            FileOpsError::SameFile(_)
        ));
    }

    #[test]
    fn a_destination_that_is_not_a_directory_is_refused() {
        let dir = TempDir::new().unwrap();
        let f = dir.path().join("file");
        std::fs::write(&f, b"x").unwrap();
        assert!(matches!(
            check_transfer(std::slice::from_ref(&f), &f).unwrap_err(),
            FileOpsError::NotADirectory(_)
        ));
    }

    #[test]
    fn an_empty_selection_is_refused_rather_than_silently_doing_nothing() {
        let dir = TempDir::new().unwrap();
        assert!(matches!(
            check_transfer(&[], dir.path()).unwrap_err(),
            FileOpsError::NoSources
        ));
    }

    /// `/tmp` is a symlink to `/private/tmp` on macOS. Comparing spellings
    /// rather than canonical paths would let the containment check through.
    #[cfg(unix)]
    #[test]
    fn containment_is_checked_after_resolving_symlinks() {
        let dir = TempDir::new().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir_all(real.join("child")).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let err = check_transfer(std::slice::from_ref(&real), &link.join("child")).unwrap_err();
        assert!(
            matches!(err, FileOpsError::DestinationInsideSource { .. }),
            "{err}"
        );
    }
}
