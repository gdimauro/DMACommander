//! Copying exactly one file, which is where the data is actually at risk.
//!
//! The invariant everything else is built on:
//!
//! > The destination name never refers to a partial file. Bytes go to
//! > `name.dmac-part`; that file is fsync'd, checked and only then `rename`d
//! > into place, and `rename` is atomic on every filesystem we support.
//!
//! So an interrupted copy leaves either the old destination or no destination —
//! never a truncated one wearing the right name. A `kill -9` leaves the part
//! file behind, and [`names::is_partial`] is how the app recognises it on the
//! next run instead of showing the user an inexplicable file. A *graceful*
//! cancel removes it, so the tree the user is looking at holds complete files
//! and nothing else.

use super::Verify;
use super::engine::Ctx;
use super::error::{FileOpsError, Result};
use super::event::{Phase, WarningKind};
use super::meta;
use super::names;
use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Bytes moved per read. Big enough that the syscall overhead disappears on a
/// fast NVMe, small enough that cancelling a 100 GB copy is felt immediately
/// and that the buffer does not show up against the RSS budget.
const CHUNK: usize = 1024 * 1024;

/// Above this, verification announces itself. Below it, a phase change per file
/// would be more messages than the whole point of coalescing allows.
const VERIFY_ANNOUNCE: u64 = 64 * 1024 * 1024;

/// What one file's copy actually did, for the caller to turn into progress,
/// warnings, or — for a move — a refusal to delete the source.
pub(crate) struct CopyReport {
    /// The source changed size or mtime while we were reading it. The bytes we
    /// wrote are a prefix of a file that is no longer the file we were asked
    /// about.
    pub(crate) changed: bool,
    pub(crate) problems: Vec<meta::MetaProblem>,
}

/// Removes the in-flight file unless the copy got all the way through.
///
/// A `Drop` guard rather than cleanup at each `return`, because there are nine
/// ways out of the function below and the tenth one added later would be the
/// one that leaks a part file.
///
/// The `is_partial` check inside it is not decoration: it is the guarantee that
/// this type can only ever unlink something wearing our own suffix, whatever a
/// caller passes in.
struct PartGuard {
    path: PathBuf,
    armed: bool,
}

impl PartGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: false }
    }
    fn arm(&mut self) {
        self.armed = true;
    }
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PartGuard {
    fn drop(&mut self) {
        if self.armed && names::is_partial(&self.path) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Copy `src` to `dst`, atomically from the destination's point of view.
///
/// `strict_snapshot` is set by moves: if the source changes underneath the copy
/// the whole entry fails, because the next thing a move would do is delete that
/// source. A plain copy tolerates it and says so.
pub(crate) fn copy_file(
    ctx: &mut Ctx,
    src: &Path,
    meta: &Metadata,
    dst: &Path,
    strict_snapshot: bool,
) -> Result<CopyReport> {
    let part = names::part_path(dst);

    // Somebody else's in-flight file, or our own from a run that was killed.
    // The two are indistinguishable from here, so neither is overwritten: one
    // of them is a job that is still running, and stealing its output file
    // corrupts both copies.
    if part.symlink_metadata().is_ok() {
        return Err(FileOpsError::io(
            &part,
            "claim the partial file (an interrupted copy or another job is using it)",
            io::Error::new(io::ErrorKind::AlreadyExists, "a partial copy is in the way"),
        ));
    }

    let mut guard = PartGuard::new(part.clone());
    let verify = ctx.verify(strict_snapshot);

    // A reflink is not a copy that happens to be fast: the destination shares
    // the source's extents, so it is byte-identical by construction and there
    // is nothing for verification to add. Only ever possible within one
    // filesystem (APFS, Btrfs, XFS, ReFS); everywhere else it fails instantly
    // and costs nothing.
    let mut reflinked = false;
    if ctx.opts.reflink && reflink_copy::reflink(src, &part).is_ok() {
        guard.arm();
        reflinked = true;
        ctx.reporter.add_bytes(meta.len());
    }

    let (copied, hash) = if reflinked {
        (meta.len(), None)
    } else {
        stream(ctx, src, meta, &part, &mut guard, verify == Verify::Hash)?
    };

    // The source is re-stat'ed *before* anything is renamed into place, so a
    // move whose source moved under it fails with the destination untouched.
    let changed = changed_since(src, meta);
    if changed && strict_snapshot {
        return Err(FileOpsError::io(
            src,
            "copy (the source changed while it was being read, so the copy is not a snapshot of it and the original will not be deleted)",
            io::Error::other("source changed during copy"),
        ));
    }

    match verify {
        Verify::None => {}
        Verify::Size => {
            let len = part
                .symlink_metadata()
                .map_err(|e| FileOpsError::io(&part, "stat for verification", e))?
                .len();
            if len != copied {
                return Err(FileOpsError::io(
                    &part,
                    "verify",
                    io::Error::other(format!("wrote {copied} bytes but the file holds {len}")),
                ));
            }
        }
        Verify::Hash => {
            if !reflinked {
                verify_hash(ctx, &part, hash, copied)?;
            }
        }
    }

    if ctx.sync_wanted(strict_snapshot) {
        // Durability before the name appears. Without this the rename can be
        // visible while the data behind it is not, which after a crash is a
        // file of the right length full of zeros — and for a move, we would
        // already have deleted the original.
        let f = File::open(&part).map_err(|e| FileOpsError::io(&part, "reopen to flush", e))?;
        f.sync_all()
            .map_err(|e| FileOpsError::io(&part, "flush to disk", e))?;
    }

    let problems = meta::apply(src, meta, &part);

    std::fs::rename(&part, dst).map_err(|e| FileOpsError::io(dst, "put into place", e))?;
    guard.disarm();

    Ok(CopyReport { changed, problems })
}

/// The read/write loop. Returns the bytes read from the source, and their hash
/// if one was asked for.
fn stream(
    ctx: &mut Ctx,
    src: &Path,
    meta: &Metadata,
    part: &Path,
    guard: &mut PartGuard,
    hashing: bool,
) -> Result<(u64, Option<blake3::Hash>)> {
    let mut fin = File::open(src).map_err(|e| FileOpsError::io(src, "open for reading", e))?;
    // `create_new` is the real defence against two jobs writing one part file;
    // the check above only makes the message better.
    let mut fout = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(part)
        .map_err(|e| FileOpsError::io(part, "create", e))?;
    guard.arm();

    // Borrowed out of the context for the duration: the loop needs the buffer
    // and the reporter at the same time, and they cannot both be `&mut ctx`.
    let mut buf = ctx.take_buffer(CHUNK);
    // Only sparse sources pay for the zero scan. On a dense 100 GB file it
    // would be pure overhead, and on Windows the platform will not say whether
    // there are holes at all, so we do not pretend to know.
    let watch_for_holes = meta::is_sparse(meta);
    let mut hasher = hashing.then(blake3::Hasher::new);

    let mut offset: u64 = 0;
    let mut written_end: u64 = 0;
    let result = loop {
        if ctx.cancelled() {
            break Err(FileOpsError::Cancelled);
        }
        let n = match read_some(&mut fin, &mut buf) {
            Ok(0) => break Ok(()),
            Ok(n) => n,
            Err(e) => break Err(FileOpsError::io(src, "read", e)),
        };
        let chunk = &buf[..n];
        if watch_for_holes && chunk.iter().all(|b| *b == 0) {
            // Seeking past a run of zeros leaves a hole, so a 4 GB sparse VM
            // image stays 4 GB of nothing instead of filling the disk.
            if let Err(e) = fout.seek(SeekFrom::Current(n as i64)) {
                break Err(FileOpsError::io(part, "seek over a hole", e));
            }
        } else if let Err(e) = fout.write_all(chunk) {
            break Err(FileOpsError::io(part, "write", e));
        } else {
            written_end = offset + n as u64;
        }
        if let Some(h) = hasher.as_mut() {
            h.update(chunk);
        }
        offset += n as u64;
        ctx.reporter.add_bytes(n as u64);
    };
    ctx.buf = buf;
    result?;

    // A file ending in a hole has no bytes to write at the end; the length has
    // to be set explicitly or the copy is short.
    if offset > written_end {
        fout.set_len(offset)
            .map_err(|e| FileOpsError::io(part, "set the final length", e))?;
    }

    let hash = hasher.map(|h| h.finalize());
    drop(fout);

    if watch_for_holes {
        // Only complain when it actually happened: a destination that kept the
        // holes deserves silence.
        if let (Some(before), Ok(after)) = (
            meta::allocated_bytes(meta),
            part.symlink_metadata().as_ref().map(meta::allocated_bytes),
        ) && let Some(after) = after
            && after > before.saturating_add(offset / 4)
        {
            ctx.reporter.warn(
                part.to_path_buf(),
                WarningKind::SparsenessLost,
                format!(
                    "the source had holes; the destination filesystem materialised {} extra bytes",
                    after.saturating_sub(before)
                ),
            );
        }
    }

    Ok((offset, hash))
}

/// One read, retrying the interruption that a signal produces. `read_exact`
/// cannot be used: a short read at the end of a file is normal, not an error.
fn read_some(f: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        match f.read(buf) {
            Ok(n) => return Ok(n),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Re-read what was written and compare it with what was read.
///
/// The honest limit of this guarantee: after `sync_all` the re-read may still
/// be served from the page cache, so what it proves is that our own pipeline —
/// buffers, offsets, hole handling — produced the right bytes. It is not a
/// media-integrity check, and nothing here claims to be one. It is still the
/// difference between "the move deleted your original" and "the move refused
/// to".
fn verify_hash(
    ctx: &mut Ctx,
    part: &Path,
    expected: Option<blake3::Hash>,
    size: u64,
) -> Result<()> {
    // No hash to compare against, having asked for one. Unreachable today, and
    // an error rather than a silent `Ok` precisely because the day it becomes
    // reachable it would otherwise let a move delete an unverified original.
    let Some(expected) = expected else {
        return Err(FileOpsError::io(
            part,
            "verify",
            io::Error::other("verification was required but no hash was taken while reading"),
        ));
    };
    let announce = size >= VERIFY_ANNOUNCE;
    if announce {
        ctx.reporter.phase(Phase::Verifying);
    }
    let mut f = File::open(part).map_err(|e| FileOpsError::io(part, "reopen to verify", e))?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = ctx.take_buffer(CHUNK);
    let result = loop {
        if ctx.cancelled() {
            break Err(FileOpsError::Cancelled);
        }
        match read_some(&mut f, &mut buf) {
            Ok(0) => break Ok(()),
            Ok(n) => hasher.update(&buf[..n]),
            Err(e) => break Err(FileOpsError::io(part, "verify", e)),
        };
    };
    ctx.buf = buf;
    result?;
    if announce {
        ctx.reporter.phase(Phase::Transferring);
    }
    if hasher.finalize() != expected {
        return Err(FileOpsError::io(
            part,
            "verify",
            io::Error::other("the copy does not match the source"),
        ));
    }
    Ok(())
}

/// Whether the source is still the file we measured.
///
/// Size *and* mtime: a log rotated in place can come back the same length, and
/// a file rewritten in place keeps its length while changing every byte.
fn changed_since(src: &Path, before: &Metadata) -> bool {
    match src.symlink_metadata() {
        Ok(now) => now.len() != before.len() || now.modified().ok() != before.modified().ok(),
        // It vanished. Whatever we copied, it is not a snapshot of something
        // that is still there.
        Err(_) => true,
    }
}

/// Recreate a symlink as a symlink.
///
/// Never as its target. Following it would turn "copy this 4 KB link" into
/// "copy the 800 GB directory it points at", and a link into `/etc` copied by
/// value is a security problem as well as a disk-space one.
/// Built at the part path and renamed into place, exactly like a file's bytes.
/// Creating a link directly at `dst` would fail whenever something is already
/// there — which is precisely the case where the user has just said "overwrite".
pub(crate) fn copy_symlink(
    src: &Path,
    dst: &Path,
    meta: &Metadata,
) -> Result<Vec<meta::MetaProblem>> {
    let target = std::fs::read_link(src).map_err(|e| FileOpsError::io(src, "read the link", e))?;
    let part = names::part_path(dst);
    if part.symlink_metadata().is_ok() {
        return Err(FileOpsError::io(
            &part,
            "claim the partial file (an interrupted copy or another job is using it)",
            io::Error::new(io::ErrorKind::AlreadyExists, "a partial copy is in the way"),
        ));
    }
    let mut guard = PartGuard::new(part.clone());
    let mut problems = Vec::new();

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&target, &part)
            .map_err(|e| FileOpsError::io(&part, "create the link", e))?;
        guard.arm();
    }
    #[cfg(windows)]
    {
        // Windows needs to know at creation time whether the link points at a
        // directory, and for a dangling link nobody can know. Guessing "file"
        // is the recoverable guess; it is also worth saying out loud.
        let points_at_dir = src.metadata().map(|m| m.is_dir()).ok();
        let r = match points_at_dir {
            Some(true) => std::os::windows::fs::symlink_dir(&target, &part),
            _ => std::os::windows::fs::symlink_file(&target, &part),
        };
        r.map_err(|e| {
            FileOpsError::io(
                &part,
                "create the link (Windows needs the SeCreateSymbolicLink privilege or Developer Mode for this)",
                e,
            )
        })?;
        guard.arm();
        if points_at_dir.is_none() {
            problems.push((
                WarningKind::SymlinkNotCreated,
                "the link was dangling, so it was recreated as a file link; if it was meant to point at a directory it will not resolve".into(),
            ));
        }
    }

    problems.extend(meta::apply_to_symlink(meta, &part));
    std::fs::rename(&part, dst).map_err(|e| FileOpsError::io(dst, "put the link into place", e))?;
    guard.disarm();
    Ok(problems)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn a_part_guard_only_ever_removes_a_part_file() {
        let dir = TempDir::new().unwrap();
        let innocent = dir.path().join("payroll.xlsx");
        std::fs::write(&innocent, b"do not delete me").unwrap();
        {
            let mut g = PartGuard::new(innocent.clone());
            g.arm();
        }
        assert!(
            innocent.exists(),
            "the guard unlinked a file that was not ours"
        );

        let part = dir.path().join("payroll.xlsx.dmac-part");
        std::fs::write(&part, b"half").unwrap();
        {
            let mut g = PartGuard::new(part.clone());
            g.arm();
        }
        assert!(!part.exists());
    }

    #[test]
    fn a_disarmed_guard_leaves_the_file_alone() {
        let dir = TempDir::new().unwrap();
        let part = dir.path().join("x.dmac-part");
        std::fs::write(&part, b"done").unwrap();
        {
            let mut g = PartGuard::new(part.clone());
            g.arm();
            g.disarm();
        }
        assert!(part.exists());
    }

    #[test]
    fn a_source_that_grows_is_detected() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("log");
        std::fs::write(&p, b"one line\n").unwrap();
        let before = p.symlink_metadata().unwrap();
        assert!(!changed_since(&p, &before));

        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(b"another line\n").unwrap();
        f.sync_all().unwrap();
        drop(f);
        assert!(
            changed_since(&p, &before),
            "a file that grew during a copy must not be reported as faithfully copied"
        );
    }

    #[test]
    fn a_source_that_vanishes_counts_as_changed() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("gone");
        std::fs::write(&p, b"x").unwrap();
        let before = p.symlink_metadata().unwrap();
        std::fs::remove_file(&p).unwrap();
        assert!(changed_since(&p, &before));
    }
}
