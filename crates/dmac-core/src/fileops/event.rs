//! What a running job says, and how often it says it.
//!
//! Two rules shaped everything here.
//!
//! **Progress is honest.** A job has two phases — scan, then transfer — and
//! during the scan there is no denominator, so [`Progress::percent`] returns
//! `None` rather than a number someone made up. A progress bar that reaches 90%
//! and stays there for four minutes is a lie the user remembers longer than the
//! copy.
//!
//! **A job over 100,000 files does not send 100,000 messages.** The counters
//! live here and are updated on every file; a message leaves at most once per
//! [`Options::progress_interval`](super::Options::progress_interval). That is
//! what lets the channel be unbounded without the queue becoming a memory leak
//! when the UI stalls: the *rate* is bounded at the source, so a UI frozen for
//! ten seconds is ten messages behind, not a million.

use super::conflict::ConflictPrompt;
use super::error::FailureKind;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

/// Identifies a job for as long as the process lives. Not persisted: nothing
/// yet survives a restart, and an id that looks durable but is not would invite
/// someone to store it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobId(pub u64);

impl JobId {
    pub(crate) fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        JobId(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// Which half of the work is running. The UI shows a different bar for each,
/// because a scan that takes a minute on a network share is not a stalled copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Counting files and bytes. No denominator exists yet.
    Scanning,
    Transferring,
    /// Re-reading what was written. Only a move insists on this, and only
    /// because it is about to delete the original.
    Verifying,
    Deleting,
}

impl std::fmt::Display for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Phase::Scanning => "scanning",
            Phase::Transferring => "copying",
            Phase::Verifying => "verifying",
            Phase::Deleting => "deleting",
        })
    }
}

/// A snapshot of where a job is. Cheap to clone; the UI keeps the last one and
/// redraws from it.
#[derive(Debug, Clone)]
pub struct Progress {
    pub phase: Phase,
    pub files_done: u64,
    /// `None` until the scan finishes. Rendering it as `0` or guessing is what
    /// produces a bar that jumps backwards.
    pub files_total: Option<u64>,
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
    /// The file actually in flight — the one thing users check when a job looks
    /// stuck.
    pub current: Option<PathBuf>,
    pub elapsed: Duration,
    /// Smoothed, over the last few reporting intervals. `None` until there is
    /// enough evidence to be worth showing.
    pub bytes_per_second: Option<u64>,
}

impl Progress {
    fn new(phase: Phase) -> Self {
        Self {
            phase,
            files_done: 0,
            files_total: None,
            bytes_done: 0,
            bytes_total: None,
            current: None,
            elapsed: Duration::ZERO,
            bytes_per_second: None,
        }
    }

    /// How far along, or `None` when nobody knows yet.
    ///
    /// Bytes when there are bytes to move, files otherwise — deleting 900 empty
    /// files is 0 bytes of work and still has honest progress. Clamped at 100
    /// because the scan's total can be *wrong*: a file that grew after it was
    /// counted really does deliver more bytes than were promised, and a bar
    /// showing 104% is a worse answer than a full one.
    pub fn percent(&self) -> Option<f64> {
        let ratio = match (self.bytes_total, self.files_total) {
            (Some(b), _) if b > 0 => self.bytes_done as f64 / b as f64,
            (_, Some(f)) if f > 0 => self.files_done as f64 / f as f64,
            // A job with a known total of zero is finished by definition.
            (Some(0), _) | (_, Some(0)) => 1.0,
            _ => return None,
        };
        Some((ratio * 100.0).clamp(0.0, 100.0))
    }

    /// Time remaining at the current rate, or `None` while that would be a
    /// guess: no total, no rate, or too early to have measured one.
    pub fn eta(&self) -> Option<Duration> {
        let total = self.bytes_total?;
        let rate = self.bytes_per_second?;
        if rate == 0 || self.elapsed < Duration::from_millis(500) {
            return None;
        }
        let left = total.saturating_sub(self.bytes_done);
        Some(Duration::from_secs_f64(left as f64 / rate as f64))
    }
}

/// Something that went wrong for one entry and did not stop the job.
///
/// Permission denied on entry 50,000 of 100,000 must not lose the other 50,000.
/// It lands here, the job keeps going, and the outcome carries the list.
#[derive(Debug, Clone)]
pub struct Failure {
    pub path: PathBuf,
    pub kind: FailureKind,
    pub message: String,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.message)
    }
}

/// Something that succeeded, but not completely faithfully.
///
/// This is where rule 3's "fail loudly rather than silently do something
/// approximate" is paid: every fidelity we could not deliver is said out loud,
/// once, with the path attached. Silence is reserved for work that was actually
/// perfect.
#[derive(Debug, Clone)]
pub struct Warning {
    pub path: PathBuf,
    pub kind: WarningKind,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarningKind {
    /// Times, permissions or xattrs did not make it across. The bytes did.
    MetadataNotPreserved,
    /// The source had holes; the destination filesystem materialised them.
    SparsenessLost,
    /// Two names for one inode arrived as two independent files. The tree is
    /// bigger than the original and edits no longer travel between them.
    HardlinkNotPreserved,
    /// A symlink could not be recreated — on Windows that usually means the
    /// account lacks the privilege, and the alternative (copying the target)
    /// would silently change what the tree means.
    SymlinkNotCreated,
    /// The name will not survive on a Windows filesystem.
    NameHazard,
    /// A moved directory could not be removed because something inside it was
    /// skipped or failed. Never forced.
    SourceKept,
    /// The platform trash refused. Nothing was deleted — a fallback to
    /// permanent deletion here is exactly the bug this whole crate is written
    /// to avoid.
    TrashUnavailable,
}

impl std::fmt::Display for Warning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.detail)
    }
}

/// How a job ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Completed,
    /// Finished, with entries that did not make it. The user has to see this;
    /// it is not a success.
    CompletedWithFailures,
    /// Stopped on request. Everything reported done *is* done — the filesystem
    /// holds complete files and nothing half-written under a real name.
    Cancelled,
    /// Stopped because continuing was impossible or unsafe.
    Aborted(String),
}

/// The last word on a job. Also arrives as the final [`JobEvent`], so a UI that
/// only listens to the channel learns everything.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub id: JobId,
    pub status: Status,
    pub files_done: u64,
    pub bytes_done: u64,
    pub skipped: u64,
    pub failures: Vec<Failure>,
    /// Failures beyond the cap. Keeping 100,000 error strings to describe a
    /// disconnected drive would be its own bug.
    pub failures_omitted: u64,
    pub warnings: Vec<Warning>,
    pub warnings_omitted: u64,
    pub elapsed: Duration,
}

impl Outcome {
    /// Whether everything asked for actually happened.
    pub fn is_clean(&self) -> bool {
        self.status == Status::Completed && self.failures.is_empty() && self.failures_omitted == 0
    }
}

/// Everything a job says. The channel is the only interface: no shared mutable
/// state, no locks held across the boundary, nothing for the UI to poll.
#[derive(Debug)]
pub enum JobEvent {
    Started(JobId),
    Progress(Progress),
    /// The engine has stopped and is waiting for an answer. Answer it or drop
    /// it — dropping is an answer too, and a safe one: the job aborts without
    /// touching the file in question.
    Conflict(ConflictPrompt),
    Warning(Warning),
    Failure(Failure),
    Finished(Outcome),
}

/// The counter block, and the thing that decides when to speak.
///
/// Not public: the engine owns exactly one, on the job's own thread, so none of
/// this needs a lock.
pub(crate) struct Reporter {
    tx: UnboundedSender<JobEvent>,
    interval: Duration,
    started: Instant,
    last_sent: Instant,
    p: Progress,
    /// `(when, bytes_done)` at the previous report, for the rate.
    mark: (Instant, u64),
    rate: Option<f64>,
    cap: usize,
    pub(crate) skipped: u64,
    failures: Vec<Failure>,
    failures_omitted: u64,
    warnings: Vec<Warning>,
    warnings_omitted: u64,
}

impl Reporter {
    pub(crate) fn new(tx: UnboundedSender<JobEvent>, interval: Duration, cap: usize) -> Self {
        let now = Instant::now();
        Self {
            tx,
            interval,
            started: now,
            // Backdated by one interval so the first update leaves immediately:
            // a job that appears to do nothing for its first 100ms looks stuck
            // on exactly the operations that finish inside 100ms.
            last_sent: now - interval,
            p: Progress::new(Phase::Scanning),
            mark: (now, 0),
            rate: None,
            cap,
            skipped: 0,
            failures: Vec::new(),
            failures_omitted: 0,
            warnings: Vec::new(),
            warnings_omitted: 0,
        }
    }

    /// Send, ignoring a closed channel.
    ///
    /// A UI that has gone away is not a reason to abandon a half-finished move;
    /// the bytes still have to land somewhere describable. The one place a
    /// closed channel *is* fatal is a conflict, which cannot be answered by
    /// nobody — see [`super::conflict`].
    pub(crate) fn emit(&self, event: JobEvent) {
        let _ = self.tx.send(event);
    }

    pub(crate) fn listening(&self) -> bool {
        !self.tx.is_closed()
    }

    pub(crate) fn phase(&mut self, phase: Phase) {
        self.p.phase = phase;
        self.flush();
    }

    pub(crate) fn set_totals(&mut self, files: u64, bytes: u64) {
        self.p.files_total = Some(files);
        self.p.bytes_total = Some(bytes);
        self.flush();
    }

    pub(crate) fn current(&mut self, path: &std::path::Path) {
        self.p.current = Some(path.to_path_buf());
        self.tick();
    }

    pub(crate) fn add_bytes(&mut self, n: u64) {
        self.p.bytes_done = self.p.bytes_done.saturating_add(n);
        self.tick();
    }

    pub(crate) fn add_files(&mut self, n: u64) {
        self.p.files_done = self.p.files_done.saturating_add(n);
        self.tick();
    }

    pub(crate) fn skip(&mut self, n: u64) {
        self.skipped = self.skipped.saturating_add(n);
    }

    /// The coalescing point. Called on every file and every chunk; sends only
    /// when the interval has passed.
    pub(crate) fn tick(&mut self) {
        if self.last_sent.elapsed() >= self.interval {
            self.flush();
        }
    }

    /// Send now, whatever the interval says. Used at phase changes and at the
    /// end, so the last thing the user sees is the true final count rather than
    /// whatever the timer happened to catch.
    pub(crate) fn flush(&mut self) {
        let now = Instant::now();
        self.p.elapsed = now.duration_since(self.started);

        let dt = now.duration_since(self.mark.0).as_secs_f64();
        if dt >= 0.05 {
            let moved = self.p.bytes_done.saturating_sub(self.mark.1) as f64;
            let instant = moved / dt;
            // Exponential smoothing. A raw instantaneous rate flickers between
            // 0 and 2 GB/s across a directory of small files and is unreadable.
            self.rate = Some(match self.rate {
                Some(prev) => prev * 0.7 + instant * 0.3,
                None => instant,
            });
            self.mark = (now, self.p.bytes_done);
        }
        self.p.bytes_per_second = self.rate.map(|r| r.max(0.0) as u64);

        self.last_sent = now;
        self.emit(JobEvent::Progress(self.p.clone()));
    }

    pub(crate) fn fail(&mut self, path: impl Into<PathBuf>, kind: FailureKind, message: String) {
        let f = Failure {
            path: path.into(),
            kind,
            message,
        };
        if self.failures.len() < self.cap {
            self.emit(JobEvent::Failure(f.clone()));
            self.failures.push(f);
        } else {
            self.failures_omitted += 1;
        }
    }

    pub(crate) fn warn(&mut self, path: impl Into<PathBuf>, kind: WarningKind, detail: String) {
        let w = Warning {
            path: path.into(),
            kind,
            detail,
        };
        if self.warnings.len() < self.cap {
            self.emit(JobEvent::Warning(w.clone()));
            self.warnings.push(w);
        } else {
            self.warnings_omitted += 1;
        }
    }

    pub(crate) fn has_failures(&self) -> bool {
        !self.failures.is_empty() || self.failures_omitted > 0
    }

    /// Close the job: one last honest progress update, then the outcome.
    pub(crate) fn finish(mut self, id: JobId, status: Status) -> Outcome {
        self.flush();
        let outcome = Outcome {
            id,
            status,
            files_done: self.p.files_done,
            bytes_done: self.p.bytes_done,
            skipped: self.skipped,
            failures: self.failures,
            failures_omitted: self.failures_omitted,
            warnings: self.warnings,
            warnings_omitted: self.warnings_omitted,
            elapsed: self.started.elapsed(),
        };
        let _ = self.tx.send(JobEvent::Finished(outcome.clone()));
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reporter() -> (Reporter, tokio::sync::mpsc::UnboundedReceiver<JobEvent>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (Reporter::new(tx, Duration::from_millis(50), 8), rx)
    }

    /// The rule this whole module exists for.
    #[test]
    fn a_hundred_thousand_files_do_not_produce_a_hundred_thousand_messages() {
        let (mut r, mut rx) = reporter();
        r.set_totals(100_000, 100_000);
        for _ in 0..100_000 {
            r.add_files(1);
            r.add_bytes(1);
        }
        r.flush();
        let mut n = 0;
        while rx.try_recv().is_ok() {
            n += 1;
        }
        assert!(n < 100, "{n} messages for 100k files is a flood");
    }

    #[test]
    fn the_final_count_is_exact_however_the_timer_fell() {
        let (mut r, mut rx) = reporter();
        for _ in 0..5000 {
            r.add_files(1);
            r.add_bytes(7);
        }
        let out = r.finish(JobId(1), Status::Completed);
        assert_eq!(out.files_done, 5000);
        assert_eq!(out.bytes_done, 35_000);
        let mut last_progress = None;
        while let Ok(e) = rx.try_recv() {
            if let JobEvent::Progress(p) = e {
                last_progress = Some(p);
            }
        }
        assert_eq!(last_progress.map(|p| p.files_done), Some(5000));
    }

    #[test]
    fn there_is_no_percentage_until_the_scan_has_finished() {
        let mut p = Progress::new(Phase::Scanning);
        p.bytes_done = 900;
        assert_eq!(p.percent(), None, "a bar drawn from nothing is a lie");
        assert_eq!(p.eta(), None);
        p.bytes_total = Some(1000);
        assert_eq!(p.percent(), Some(90.0));
    }

    /// The scan's total is a measurement, not a promise: a file that grew after
    /// it was counted delivers more bytes than were expected.
    #[test]
    fn a_total_that_turns_out_to_be_wrong_does_not_produce_a_bar_past_full() {
        let mut p = Progress::new(Phase::Transferring);
        p.bytes_total = Some(100);
        p.bytes_done = 250;
        assert_eq!(p.percent(), Some(100.0));
    }

    #[test]
    fn a_job_that_moves_no_bytes_still_has_honest_progress() {
        let mut p = Progress::new(Phase::Deleting);
        p.bytes_total = Some(0);
        p.files_total = Some(4);
        p.files_done = 1;
        assert_eq!(p.percent(), Some(25.0));
    }

    #[test]
    fn failures_are_capped_but_counted() {
        let (mut r, _rx) = reporter();
        for i in 0..20 {
            r.fail(
                format!("/tmp/{i}"),
                FailureKind::PermissionDenied,
                "denied".into(),
            );
        }
        let out = r.finish(JobId(1), Status::CompletedWithFailures);
        assert_eq!(out.failures.len(), 8);
        assert_eq!(out.failures_omitted, 12);
        assert!(!out.is_clean());
    }

    /// A UI that closed its receiver must not take the job down with it.
    #[test]
    fn a_dropped_receiver_does_not_stop_the_counters() {
        let (mut r, rx) = reporter();
        drop(rx);
        r.add_files(3);
        r.flush();
        assert!(!r.listening());
        let out = r.finish(JobId(1), Status::Completed);
        assert_eq!(out.files_done, 3);
    }
}
