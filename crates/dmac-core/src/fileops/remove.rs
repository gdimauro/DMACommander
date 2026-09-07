//! Deleting things, which is the only operation with no undo of its own.
//!
//! Two rules hold this module up.
//!
//! **The trash is the default and there is no fallback out of it.** If the
//! platform trash refuses — a network volume with no `.Trashes`, a container
//! with no session bus — the file stays exactly where it is and the job says
//! why. Quietly deleting forever because the recoverable path was unavailable
//! would turn "I can get that back" into "it is gone", which is the single
//! worst thing this program could do.
//!
//! **A symlink is unlinked, never followed.** A tree holding a link to
//! `~/Documents` must lose the link and nothing else. `std::fs::remove_dir_all`
//! is not used here, so that this behaviour is ours, visible, and tested,
//! rather than a property of whichever version of the standard library happens
//! to be compiled in.

use super::engine::Ctx;
use super::error::{FileOpsError, Result};
use super::event::WarningKind;
use std::path::{Path, PathBuf};

/// Where a deleted file goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeleteMode {
    /// The platform's recoverable bin. The default, everywhere, always.
    #[default]
    Trash,
    /// Gone. Only ever reached because the user explicitly asked for it —
    /// Shift+F8, not F8.
    Permanent,
}

/// The platform trash, behind a trait.
///
/// Not for elegance: it is so the "trash refused, so nothing was deleted" rule
/// can be *tested*, which is impossible against the real trash without putting
/// a test's rubbish into the developer's own bin. A test that leaves files
/// outside its `TempDir` is a defect, and that includes `~/.Trash`.
pub trait TrashCan: std::fmt::Debug + Send + Sync {
    /// Move `path` to the platform's recoverable bin.
    fn trash(&self, path: &Path) -> std::result::Result<(), String>;
}

/// The real one.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemTrash;

impl TrashCan for SystemTrash {
    fn trash(&self, path: &Path) -> std::result::Result<(), String> {
        trash::delete(path).map_err(|e| e.to_string())
    }
}

/// Refuse anything that is not a nameable child of some directory.
///
/// `/`, `C:\` and `/foo/..` all fail here. The guard is cheap and it runs
/// before a single `unlink`, so a bug that produced a root path upstream ends
/// as an error message rather than as an empty disk.
pub(crate) fn check_removable(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() || path.parent().is_none() || path.file_name().is_none() {
        return Err(FileOpsError::RootPath(path.to_path_buf()));
    }
    Ok(())
}

/// Send one top-level target to the trash.
///
/// The trash takes whole trees itself, so there is no walk here — which is also
/// why a trashed directory reports as one item rather than as its contents.
pub(crate) fn to_trash(ctx: &mut Ctx, path: &Path) -> Result<()> {
    check_removable(path)?;
    ctx.reporter.current(path);
    match ctx.trash().trash(path) {
        Ok(()) => Ok(()),
        Err(why) => {
            ctx.reporter.warn(
                path.to_path_buf(),
                WarningKind::TrashUnavailable,
                format!("nothing was deleted: {why}"),
            );
            Err(FileOpsError::io(
                path,
                "move to the trash (nothing was deleted; use permanent delete deliberately if that is what you want)",
                std::io::Error::other(why),
            ))
        }
    }
}

/// One step of the post-order walk.
enum Step {
    /// Look at this path: unlink it, or descend into it.
    Enter(PathBuf),
    /// Its children have been dealt with; remove the directory itself.
    Leave(PathBuf),
}

/// Remove a tree permanently, depth-first, children before parents.
///
/// An explicit stack rather than recursion: a tree can be thousands deep (an
/// extracted archive, a node_modules from 2016), and blowing the stack in the
/// middle of a delete leaves a half-removed tree with no report of what
/// happened.
///
/// Per-entry failures do not stop the walk. Permission denied on entry 50,000
/// of 100,000 must not cost the user the other 50,000 deletions they asked for.
pub(crate) fn remove_tree(ctx: &mut Ctx, root: &Path) -> Result<()> {
    check_removable(root)?;
    let mut stack = vec![Step::Enter(root.to_path_buf())];

    while let Some(step) = stack.pop() {
        if ctx.cancelled() {
            return Err(FileOpsError::Cancelled);
        }
        match step {
            Step::Enter(path) => {
                // `symlink_metadata`, always. `metadata` would report a link to
                // a directory as a directory and send this walk out of the tree
                // and into whatever it points at.
                let meta = match path.symlink_metadata() {
                    Ok(m) => m,
                    Err(e) => {
                        ctx.fail_io(&path, "read", e);
                        continue;
                    }
                };
                if meta.is_dir() {
                    ctx.reporter.current(&path);
                    let rd = match std::fs::read_dir(&path) {
                        Ok(rd) => rd,
                        Err(e) => {
                            ctx.fail_io(&path, "list", e);
                            continue;
                        }
                    };
                    stack.push(Step::Leave(path.clone()));
                    for entry in rd {
                        match entry {
                            Ok(e) => stack.push(Step::Enter(e.path())),
                            Err(e) => ctx.fail_io(&path, "list", e),
                        }
                    }
                } else {
                    let size = meta.len();
                    match remove_one_file(&path) {
                        Ok(()) => {
                            ctx.reporter.add_files(1);
                            if !meta.is_symlink() {
                                ctx.reporter.add_bytes(size);
                            }
                        }
                        Err(e) => ctx.fail_io(&path, "delete", e),
                    }
                }
            }
            Step::Leave(path) => {
                // `remove_dir`, not `remove_dir_all`: it fails on a directory
                // that still holds something, which is exactly the behaviour
                // wanted when a child was skipped or failed. Forcing it would
                // destroy the thing the earlier failure was protecting.
                if let Err(e) = std::fs::remove_dir(&path) {
                    if e.kind() == std::io::ErrorKind::DirectoryNotEmpty {
                        ctx.reporter.warn(
                            path.clone(),
                            WarningKind::SourceKept,
                            "kept: it still holds entries that could not be deleted".into(),
                        );
                    } else {
                        ctx.fail_io(&path, "delete the directory", e);
                    }
                }
            }
        }
    }
    Ok(())
}

/// Unlink one non-directory entry, including a symlink (as itself).
fn remove_one_file(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        #[cfg(windows)]
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            // A read-only file on Windows cannot be deleted until the attribute
            // is cleared. `del` has the same rule and everybody hits it once.
            let mut perms = match path.symlink_metadata() {
                Ok(m) => m.permissions(),
                Err(_) => return Err(e),
            };
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            if std::fs::set_permissions(path, perms).is_err() {
                return Err(e);
            }
            std::fs::remove_file(path)
        }
        Err(e) => Err(e),
    }
}

/// Remove a source directory after a move, but only if it came out empty.
///
/// Never `remove_dir_all`. If anything is left inside — a file the user chose
/// to skip, one that failed to copy — the directory stays and the job says so.
/// The whole point of a verified move is that nothing is deleted that was not
/// first copied.
pub(crate) fn remove_moved_directory(ctx: &mut Ctx, path: &Path) {
    if check_removable(path).is_err() {
        return;
    }
    match std::fs::remove_dir(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
            ctx.reporter.warn(
                path.to_path_buf(),
                WarningKind::SourceKept,
                "kept: something inside it was not moved".into(),
            );
        }
        Err(e) => ctx.fail_io(path, "remove the moved directory", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filesystem_root_is_never_removable() {
        assert!(matches!(
            check_removable(Path::new("/")),
            Err(FileOpsError::RootPath(_))
        ));
        assert!(matches!(
            check_removable(Path::new("")),
            Err(FileOpsError::RootPath(_))
        ));
        // `/tmp/..` has no file name of its own — it is the root by another
        // spelling, and a bug upstream could produce it.
        assert!(matches!(
            check_removable(Path::new("/tmp/..")),
            Err(FileOpsError::RootPath(_))
        ));
        assert!(check_removable(Path::new("/tmp/something")).is_ok());
    }
}
