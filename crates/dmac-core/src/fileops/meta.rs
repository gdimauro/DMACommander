//! Everything about a file that is not its bytes.
//!
//! A copy that loses the executable bit, the modification time or the extended
//! attributes is not a copy — it is a new file with the same contents, and the
//! difference shows up later as a script that will not run, a backup that looks
//! entirely rewritten, or a macOS file that lost its resource fork.
//!
//! What is preserved, and what is not:
//!
//! | | preserved | how |
//! |---|---|---|
//! | mtime, atime | yes | `filetime`, after the bytes are in place |
//! | permissions | yes | Unix mode; on Windows only the read-only bit exists |
//! | xattrs | yes, on Unix | covers macOS resource forks and quarantine, and Linux `user.*` |
//! | hardlink topology | yes, within one job, on Unix | second and later names become links, not copies |
//! | symlinks | yes, as links | never followed by default |
//! | sparseness | best effort | zero runs are seeked over, not written |
//! | btime (creation time) | **no** | there is no portable way to set it; said here once rather than warned 100,000 times |
//! | ACLs | **no** | POSIX draft ACLs and Windows DACLs need per-platform code that is not written yet |
//!
//! The two "no"s are the honest part of this table. They are in the module
//! documentation instead of being quietly omitted so that whoever needs them
//! knows they are missing rather than discovering it from a user.

use super::event::WarningKind;
use std::fs::Metadata;
use std::path::Path;

/// A fidelity we could not deliver, to be reported against the file.
pub(crate) type MetaProblem = (WarningKind, String);

/// Copy times, permissions and extended attributes from `src` onto `dst`.
///
/// Never fails the copy. The bytes are already there and correct; losing a
/// timestamp is worth a warning, not a lost file. Every loss is returned, and
/// the caller reports it.
pub(crate) fn apply(src: &Path, meta: &Metadata, dst: &Path) -> Vec<MetaProblem> {
    let mut problems = Vec::new();

    // xattrs first: on macOS this is where the resource fork and the quarantine
    // flag live, and both should be in place before the file becomes visible
    // under its real name.
    copy_xattrs(src, dst, &mut problems);

    // Permissions after the content, never before: a read-only source would
    // otherwise leave us unable to finish writing our own part file.
    if let Err(e) = std::fs::set_permissions(dst, meta.permissions()) {
        problems.push((
            WarningKind::MetadataNotPreserved,
            format!("permissions were not copied: {e}"),
        ));
    }

    // Times last, because everything above touches ctime and writing touches
    // mtime.
    let atime = filetime::FileTime::from_last_access_time(meta);
    let mtime = filetime::FileTime::from_last_modification_time(meta);
    if let Err(e) = filetime::set_file_times(dst, atime, mtime) {
        problems.push((
            WarningKind::MetadataNotPreserved,
            format!("timestamps were not copied: {e}"),
        ));
    }

    problems
}

/// Times for a symlink, set on the link itself rather than on what it points
/// at. Following it would stamp the target — a file outside the tree we were
/// asked about.
pub(crate) fn apply_to_symlink(meta: &Metadata, dst: &Path) -> Vec<MetaProblem> {
    let atime = filetime::FileTime::from_last_access_time(meta);
    let mtime = filetime::FileTime::from_last_modification_time(meta);
    match filetime::set_symlink_file_times(dst, atime, mtime) {
        Ok(()) => Vec::new(),
        Err(e) => vec![(
            WarningKind::MetadataNotPreserved,
            format!("link timestamps were not copied: {e}"),
        )],
    }
}

#[cfg(unix)]
fn copy_xattrs(src: &Path, dst: &Path, problems: &mut Vec<MetaProblem>) {
    let names = match xattr::list(src) {
        Ok(n) => n,
        // A filesystem without xattr support says so; there is nothing to lose
        // and nothing to report.
        Err(_) => return,
    };
    let mut lost: Vec<String> = Vec::new();
    for name in names {
        match xattr::get(src, &name) {
            Ok(Some(value)) => {
                if xattr::set(dst, &name, &value).is_err() {
                    lost.push(name.to_string_lossy().into_owned());
                }
            }
            Ok(None) => {}
            Err(_) => lost.push(name.to_string_lossy().into_owned()),
        }
    }
    if !lost.is_empty() {
        // One warning per file, not one per attribute: a quarantined download
        // tree would otherwise produce three warnings for every file in it.
        problems.push((
            WarningKind::MetadataNotPreserved,
            format!("extended attributes were not copied: {}", lost.join(", ")),
        ));
    }
}

#[cfg(not(unix))]
fn copy_xattrs(_src: &Path, _dst: &Path, _problems: &mut Vec<MetaProblem>) {
    // NTFS alternate data streams are the closest equivalent and need
    // `windows-sys` to enumerate. Not written yet, and not silently pretended
    // either — see the table in this module's documentation.
}

/// Whether two paths name the same file on disk.
///
/// This is the check that stops `rename foo -> FOO` from destroying the file on
/// a case-insensitive filesystem. Without it the engine sees "the destination
/// exists", the user answers "overwrite", the destination is removed — and the
/// destination *was* the source, so the rename that follows has nothing left to
/// rename. macOS and Windows are case-insensitive by default, so this is the
/// common path, not the exotic one.
pub(crate) fn same_file(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(x), Ok(y)) = (a.symlink_metadata(), b.symlink_metadata()) {
            return x.dev() == y.dev() && x.ino() == y.ino();
        }
        false
    }
    #[cfg(not(unix))]
    {
        // No inode numbers without `windows-sys`. Canonicalising resolves the
        // case, the short-name form and any symlinks in the parents, which
        // covers the case-only rename this exists for.
        match (a.canonicalize(), b.canonicalize()) {
            (Ok(x), Ok(y)) => x == y,
            _ => false,
        }
    }
}

/// The identity of an inode, for files that have more than one name.
///
/// `None` for anything with a single link — the map only needs entries that can
/// collide, and inserting 100,000 unshared files would cost more memory than
/// the copy itself.
#[cfg(unix)]
pub(crate) fn hardlink_key(meta: &Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    (meta.nlink() > 1).then(|| (meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
pub(crate) fn hardlink_key(_meta: &Metadata) -> Option<(u64, u64)> {
    // Requires GetFileInformationByHandle. Until that is written, a hardlinked
    // tree copies as independent files on Windows and the job says so.
    None
}

/// Bytes actually allocated on disk, when the platform will say.
///
/// A file whose allocation is well under its length has holes in it. Copying it
/// naively fills them with real zeros, and a 4 GB sparse VM image becomes a
/// 4 GB real one on a disk that had 200 MB free.
#[cfg(unix)]
pub(crate) fn allocated_bytes(meta: &Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(meta.blocks().saturating_mul(512))
}

#[cfg(not(unix))]
pub(crate) fn allocated_bytes(_meta: &Metadata) -> Option<u64> {
    None
}

/// Whether the source has holes worth preserving.
///
/// The slack allows for tail padding and for filesystems that compress; only a
/// file allocating meaningfully less than its length counts.
pub(crate) fn is_sparse(meta: &Metadata) -> bool {
    match allocated_bytes(meta) {
        Some(allocated) => meta.len() > 4096 && allocated + 4096 < meta.len(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn metadata_travels_with_the_bytes() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        std::fs::write(&src, b"hello").unwrap();
        std::fs::write(&dst, b"hello").unwrap();

        let past = filetime::FileTime::from_unix_time(1_000_000_000, 0);
        filetime::set_file_times(&src, past, past).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o750)).unwrap();
        }

        let meta = src.symlink_metadata().unwrap();
        let problems = apply(&src, &meta, &dst);
        assert!(problems.is_empty(), "{problems:?}");

        let got = dst.symlink_metadata().unwrap();
        assert_eq!(
            filetime::FileTime::from_last_modification_time(&got),
            past,
            "an mtime that changes makes every backup look rewritten"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(got.permissions().mode() & 0o777, 0o750);
        }
    }

    #[cfg(unix)]
    #[test]
    fn extended_attributes_travel_too() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        std::fs::write(&src, b"x").unwrap();
        std::fs::write(&dst, b"x").unwrap();
        // Skip where the temp filesystem has no xattr support rather than
        // failing for a reason that has nothing to do with the code.
        if xattr::set(&src, "user.dmac.test", b"v").is_err() {
            return;
        }
        let meta = src.symlink_metadata().unwrap();
        apply(&src, &meta, &dst);
        assert_eq!(
            xattr::get(&dst, "user.dmac.test").unwrap().as_deref(),
            Some(&b"v"[..])
        );
    }

    /// The one that stops `foo` -> `FOO` from deleting the file on macOS and
    /// Windows.
    #[test]
    fn a_case_only_rename_is_recognised_as_the_same_file() {
        let dir = TempDir::new().unwrap();
        let lower = dir.path().join("case.txt");
        std::fs::write(&lower, b"payload").unwrap();
        let upper = dir.path().join("CASE.TXT");

        if upper.symlink_metadata().is_err() {
            // A case-sensitive filesystem: they really are two different names.
            assert!(!same_file(&lower, &upper));
            return;
        }
        assert!(
            same_file(&lower, &upper),
            "the destination is the source under another spelling"
        );
    }

    #[test]
    fn two_different_files_are_not_the_same_file() {
        let dir = TempDir::new().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"same bytes").unwrap();
        std::fs::write(&b, b"same bytes").unwrap();
        assert!(!same_file(&a, &b), "identical contents are not identity");
    }

    #[cfg(unix)]
    #[test]
    fn only_files_with_more_than_one_name_enter_the_hardlink_map() {
        let dir = TempDir::new().unwrap();
        let a = dir.path().join("a");
        std::fs::write(&a, b"x").unwrap();
        assert_eq!(hardlink_key(&a.symlink_metadata().unwrap()), None);
        let b = dir.path().join("b");
        std::fs::hard_link(&a, &b).unwrap();
        let ka = hardlink_key(&a.symlink_metadata().unwrap());
        let kb = hardlink_key(&b.symlink_metadata().unwrap());
        assert!(ka.is_some());
        assert_eq!(ka, kb, "both names are one inode");
    }

    #[test]
    fn a_file_with_holes_is_recognised_where_the_platform_says() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("sparse.img");
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(8 * 1024 * 1024).unwrap();
        drop(f);
        let meta = path.symlink_metadata().unwrap();
        if allocated_bytes(&meta).is_none() {
            return; // Windows: no answer available, and none invented.
        }
        assert!(is_sparse(&meta), "8MB of nothing should allocate nothing");

        let dense = dir.path().join("dense.bin");
        let mut f = std::fs::File::create(&dense).unwrap();
        f.write_all(&vec![7u8; 8 * 1024 * 1024]).unwrap();
        f.sync_all().unwrap();
        drop(f);
        assert!(!is_sparse(&dense.symlink_metadata().unwrap()));
    }
}
