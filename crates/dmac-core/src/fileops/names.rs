//! Filenames: the part of a file operation that is different on every platform
//! and wrong in a different way on each.
//!
//! Everything here is a pure function over strings and paths, with no I/O, for
//! one reason: the rules that matter are the *other* platform's rules. A macOS
//! developer needs to be able to run — and fail — the Windows reserved-name
//! test without a Windows machine, or the rule gets written once, never
//! executed, and discovered by a user whose copy silently created a file they
//! can never open.

use std::path::{Path, PathBuf};

/// The suffix every incomplete destination file wears.
///
/// The contract this buys: **a file without this suffix is complete.** If the
/// process is killed mid-copy — `kill -9`, a power cut, a closed laptop — the
/// bytes that were in flight are in a file named `…dmac-part`, and the file the
/// user asked for either does not exist yet or is the old, whole one. Nothing
/// in between is ever visible under the real name, because the real name only
/// ever appears via `rename`, which is atomic.
pub const PART_SUFFIX: &str = ".dmac-part";

/// Longest filename most filesystems accept, in bytes. ext4, APFS, NTFS and
/// XFS all sit at 255; the part-file name has to stay under it even for a
/// source that is already at the limit.
const NAME_MAX: usize = 255;

/// Where the in-flight bytes for `dst` are written.
///
/// Normally `dst` plus the suffix. For a name already close to the filesystem's
/// limit the base is shortened instead of overflowing it — the part name is
/// ours and temporary, the name that has to be exact is the one it is renamed
/// to at the end.
pub fn part_path(dst: &Path) -> PathBuf {
    let name = dst.file_name().unwrap_or_default().to_string_lossy();
    let mut out = dst.to_path_buf();
    out.set_file_name(part_name(&name));
    out
}

/// The part-file name for `name`, kept under [`NAME_MAX`].
fn part_name(name: &str) -> String {
    if name.len() + PART_SUFFIX.len() <= NAME_MAX {
        return format!("{name}{PART_SUFFIX}");
    }
    // Trim on a char boundary: slicing a multi-byte character in half produces
    // a name that is not valid UTF-8, and the error you get back names the
    // wrong problem.
    let budget = NAME_MAX - PART_SUFFIX.len();
    let mut cut = budget;
    while cut > 0 && !name.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{PART_SUFFIX}", &name[..cut])
}

/// Whether this path is one of our in-flight files.
///
/// The panel uses it to explain a leftover after a crash instead of showing an
/// inexplicable file, and [`crate::fileops`] uses it as the guard on every
/// unlink it performs during cleanup: nothing without this suffix is ever
/// removed by the part-file machinery.
pub fn is_partial(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(PART_SUFFIX) && n.len() > PART_SUFFIX.len())
}

/// Split a filename into stem and extension the way [`crate::Entry::extension`]
/// does, so a rename produced here matches what the panel would show.
fn split_extension(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(0) | None => (name, ""),
        Some(i) => (&name[..i], &name[i..]),
    }
}

/// How many `name (n)` candidates to try before giving up.
///
/// A bound rather than a loop to infinity: a destination directory that somehow
/// holds every candidate should produce an error the user can read, not a job
/// that appears to hang.
const RENAME_ATTEMPTS: u32 = 10_000;

/// The first free `name (2)`, `name (3)`, … next to `dst`.
///
/// `exists` is injected rather than called directly so the numbering rule can
/// be tested without a filesystem — and so the caller decides whether "exists"
/// means `symlink_metadata` (it does: a dangling symlink occupies the name just
/// as firmly as a file does, and `create_new` on it fails).
pub fn unique_name(dst: &Path, exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let name = dst.file_name()?.to_string_lossy().into_owned();
    let (stem, ext) = split_extension(&name);
    let dir = dst.parent()?;
    for n in 2..RENAME_ATTEMPTS {
        let candidate = dir.join(format!("{stem} ({n}){ext}"));
        if !exists(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// A filename that this platform, or a platform the file is heading for, will
/// not store faithfully.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameHazard {
    /// `con`, `aux`, `nul`, `com3`… — reserved by the Win32 device namespace
    /// with *any* extension, so `con.txt` is reserved too. Creating one either
    /// fails or opens a device.
    ReservedDevice,
    /// `<>:"/\|?*` or a control character. Windows rejects these outright;
    /// worse, some network filesystems substitute them silently.
    IllegalCharacter,
    /// Windows strips trailing dots and spaces without telling you, so `a.` and
    /// `a ` both become `a` — two entries in a copied directory collapse into
    /// one and the second overwrites the first.
    TrailingDotOrSpace,
}

impl std::fmt::Display for NameHazard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            NameHazard::ReservedDevice => "reserved as a device name on Windows",
            NameHazard::IllegalCharacter => "contains a character Windows cannot store",
            NameHazard::TrailingDotOrSpace => "ends in a dot or space, which Windows strips",
        };
        f.write_str(s)
    }
}

/// Names the Win32 device namespace owns, whatever extension follows.
const RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// What Windows would do to this name, if anything.
///
/// Evaluated on every platform, not just Windows, because the answer is what
/// tells a Linux user that the tree they are copying to a mounted SMB share
/// will not arrive intact. On Windows itself the caller refuses the entry; on
/// Unix it is only reported when the destination is known to be foreign.
pub fn windows_hazard(name: &str) -> Option<NameHazard> {
    if name.is_empty() {
        return None;
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Some(NameHazard::TrailingDotOrSpace);
    }
    if name
        .chars()
        .any(|c| matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c < ' ')
    {
        return Some(NameHazard::IllegalCharacter);
    }
    // `con.txt` is `con`. The stem is what the device namespace matches on, and
    // it matches case-insensitively.
    let stem = name.split('.').next().unwrap_or(name).to_ascii_lowercase();
    if RESERVED.contains(&stem.as_str()) {
        return Some(NameHazard::ReservedDevice);
    }
    None
}

/// Why a name typed by the user cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    #[error("the name is empty")]
    Empty,
    #[error("`{0}` is a path, not a name — it contains a separator")]
    HasSeparator(String),
    #[error("`{0}` is a relative path component, not a name")]
    Traversal(String),
    #[error("the name contains a NUL byte")]
    Nul,
    #[error("`{name}` is {hazard}")]
    Hazard { name: String, hazard: NameHazard },
}

/// The chokepoint every user-typed name goes through.
///
/// Rename and make-directory both take a *name*, never a path, and this is what
/// enforces that. Without it, typing `../../.ssh/authorized_keys` into the
/// rename box would move a file somewhere the user is not looking — the same
/// class of hole as zip-slip, reached through the keyboard instead of an
/// archive. This mirrors `VfsPath::join`'s rule deliberately; the two must not
/// disagree about what a single path component is.
///
/// Windows hazards are rejected only on Windows: `aux.c` is a perfectly ordinary
/// filename on Linux and refusing to create it there would be a bug, not a
/// safeguard.
pub fn validate_component(name: &str) -> Result<(), NameError> {
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if name.contains('\0') {
        return Err(NameError::Nul);
    }
    if name.contains('/') || name.contains('\\') {
        return Err(NameError::HasSeparator(name.to_string()));
    }
    if name == "." || name == ".." {
        return Err(NameError::Traversal(name.to_string()));
    }
    if cfg!(windows)
        && let Some(hazard) = windows_hazard(name)
    {
        return Err(NameError::Hazard {
            name: name.to_string(),
            hazard,
        });
    }
    Ok(())
}

/// The same rule for a name that may legitimately be several components deep —
/// the make-directory box, where typing `a/b/c` and getting three levels is the
/// behaviour people expect from every orthodox file manager.
///
/// Still refuses anything that could leave the parent: absolute paths, roots,
/// and `..` at any depth.
pub fn validate_relative(name: &str) -> Result<(), NameError> {
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if name.contains('\0') {
        return Err(NameError::Nul);
    }
    let p = Path::new(name);
    if p.is_absolute() || name.starts_with('/') || name.starts_with('\\') {
        return Err(NameError::Traversal(name.to_string()));
    }
    for c in p.components() {
        match c {
            std::path::Component::Normal(part) => {
                let part = part.to_string_lossy();
                if cfg!(windows)
                    && let Some(hazard) = windows_hazard(&part)
                {
                    return Err(NameError::Hazard {
                        name: part.into_owned(),
                        hazard,
                    });
                }
            }
            _ => return Err(NameError::Traversal(name.to_string())),
        }
    }
    Ok(())
}

/// Rewrite an absolute Windows path into its `\\?\` verbatim form, which is the
/// only way to address something longer than 260 characters.
///
/// Takes and returns `&str` so the rule is testable on every platform — the bug
/// this prevents ("cannot find the path specified" at depth on a Windows box)
/// is not reproducible on the machine most of this is written on.
///
/// `None` means "use the path as it is": already verbatim, or relative, in
/// which case the caller has nothing better to do than pass it through. The
/// verbatim namespace does no `.`/`..` resolution at all, so handing it a
/// relative path would address something else entirely.
pub fn windows_verbatim(path: &str) -> Option<String> {
    if path.starts_with(r"\\?\") || path.starts_with(r"\\.\") {
        return None;
    }
    if path.contains('/') {
        // A mixed path is normal in Rust code; verbatim form accepts only
        // backslashes, and silently addressing the wrong file is not on the
        // table. Normalise, then continue.
        return windows_verbatim(&path.replace('/', r"\"));
    }
    if path.split('\\').any(|c| c == "." || c == "..") {
        return None;
    }
    let bytes = path.as_bytes();
    let drive =
        bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\';
    if drive {
        return Some(format!(r"\\?\{path}"));
    }
    if let Some(rest) = path.strip_prefix(r"\\") {
        // A UNC share: \\server\share -> \\?\UNC\server\share
        return Some(format!(r"\\?\UNC\{rest}"));
    }
    None
}

/// Where the legacy Win32 path limit starts to bite. `MAX_PATH` is 260 and the
/// margin leaves room for the names this job will append to it.
const WINDOWS_PATH_LIMIT: usize = 240;

/// The form of `path` this platform can actually open.
///
/// On Windows a path near the legacy limit is rewritten into its `\\?\`
/// verbatim form, which is the only way to address anything deeper — without
/// it, a perfectly ordinary `node_modules` tree copies until it hits "the
/// system cannot find the path specified" and the user has no idea why. Short
/// paths are returned untouched, so the ordinary case behaves exactly as it
/// always has and the verbatim namespace's stricter rules never surprise anyone.
///
/// Everywhere else this is the identity function.
///
/// Applied once, to the job's roots. Every path below them is built by joining,
/// and a join onto a verbatim path stays verbatim.
pub fn addressable(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        if path.as_os_str().len() >= WINDOWS_PATH_LIMIT
            && let Some(s) = path.to_str()
            && let Some(v) = windows_verbatim(s)
        {
            return PathBuf::from(v);
        }
        path.to_path_buf()
    }
    #[cfg(not(windows))]
    {
        let _ = WINDOWS_PATH_LIMIT;
        path.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_part_file_is_the_destination_plus_the_suffix() {
        let p = part_path(Path::new("/tmp/a/photo.jpg"));
        assert_eq!(p, PathBuf::from("/tmp/a/photo.jpg.dmac-part"));
        assert!(is_partial(&p));
        assert!(!is_partial(Path::new("/tmp/a/photo.jpg")));
    }

    /// A source already at the filesystem's name limit must still be copyable;
    /// the part name is ours, so it is the one that gives ground.
    #[test]
    fn a_maximal_name_still_gets_a_part_file_that_fits() {
        let name = "x".repeat(255);
        let p = part_path(&PathBuf::from("/tmp").join(&name));
        let got = p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(got.len() <= 255, "{} bytes", got.len());
        assert!(is_partial(&p));
    }

    #[test]
    fn shortening_a_part_name_never_splits_a_character() {
        // 128 two-byte characters = 256 bytes, one over the limit.
        let name = "é".repeat(128);
        let p = part_path(&PathBuf::from("/tmp").join(&name));
        let got = p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(got.len() <= 255);
        assert!(got.ends_with(PART_SUFFIX));
        assert!(!got.contains('\u{fffd}'), "the name was cut mid-character");
    }

    /// A bare `.dmac-part` with nothing in front of it is somebody else's file,
    /// not evidence of our own interrupted copy.
    #[test]
    fn the_suffix_alone_is_not_a_part_file() {
        assert!(!is_partial(Path::new("/tmp/.dmac-part")));
    }

    #[test]
    fn auto_rename_counts_up_and_keeps_the_extension() {
        let taken = ["/d/a.txt", "/d/a (2).txt"];
        let got = unique_name(Path::new("/d/a.txt"), |p| {
            taken.contains(&p.to_string_lossy().as_ref())
        });
        assert_eq!(got, Some(PathBuf::from("/d/a (3).txt")));
    }

    #[test]
    fn auto_rename_treats_a_dotfile_as_having_no_extension() {
        let got = unique_name(Path::new("/d/.bashrc"), |_| false);
        assert_eq!(got, Some(PathBuf::from("/d/.bashrc (2)")));
    }

    #[test]
    fn windows_device_names_are_recognised_with_any_extension() {
        assert_eq!(windows_hazard("con"), Some(NameHazard::ReservedDevice));
        assert_eq!(windows_hazard("CON.txt"), Some(NameHazard::ReservedDevice));
        assert_eq!(
            windows_hazard("aux.tar.gz"),
            Some(NameHazard::ReservedDevice)
        );
        assert_eq!(windows_hazard("com9"), Some(NameHazard::ReservedDevice));
        assert_eq!(
            windows_hazard("console"),
            None,
            "only exact stems are devices"
        );
        assert_eq!(windows_hazard("com10"), None);
    }

    #[test]
    fn windows_rejects_trailing_dots_spaces_and_colons() {
        assert_eq!(
            windows_hazard("report."),
            Some(NameHazard::TrailingDotOrSpace)
        );
        assert_eq!(
            windows_hazard("report "),
            Some(NameHazard::TrailingDotOrSpace)
        );
        assert_eq!(
            windows_hazard("a:b"),
            Some(NameHazard::IllegalCharacter),
            "an NTFS alternate data stream, not a filename"
        );
        assert_eq!(
            windows_hazard("bell\u{7}"),
            Some(NameHazard::IllegalCharacter)
        );
        assert_eq!(windows_hazard("perfectly.fine"), None);
    }

    #[test]
    fn a_typed_name_cannot_be_a_path() {
        assert_eq!(
            validate_component("../../etc/passwd"),
            Err(NameError::HasSeparator("../../etc/passwd".into()))
        );
        assert_eq!(
            validate_component(".."),
            Err(NameError::Traversal("..".into()))
        );
        assert_eq!(validate_component(""), Err(NameError::Empty));
        assert_eq!(validate_component("a\0b"), Err(NameError::Nul));
        assert!(validate_component("notes.txt").is_ok());
    }

    #[test]
    fn make_directory_accepts_depth_but_never_escape() {
        assert!(validate_relative("a/b/c").is_ok());
        assert!(validate_relative("../sibling").is_err());
        assert!(validate_relative("a/../../b").is_err());
        assert!(validate_relative("/etc").is_err());
        assert!(validate_relative(r"\\server\share").is_err());
    }

    /// `aux.c` is an ordinary C file on Linux. Refusing it there would be us
    /// exporting somebody else's limitation.
    #[test]
    fn windows_hazards_are_only_fatal_on_windows() {
        let got = validate_component("aux.c");
        if cfg!(windows) {
            assert!(got.is_err());
        } else {
            assert!(got.is_ok());
        }
    }

    #[test]
    fn long_windows_paths_become_verbatim() {
        assert_eq!(windows_verbatim(r"C:\a\b"), Some(r"\\?\C:\a\b".to_string()));
        assert_eq!(
            windows_verbatim(r"\\srv\share\x"),
            Some(r"\\?\UNC\srv\share\x".to_string())
        );
        assert_eq!(windows_verbatim(r"\\?\C:\a"), None, "already verbatim");
        assert_eq!(windows_verbatim(r"a\b"), None, "relative stays as it is");
    }

    /// The verbatim namespace resolves nothing, so `..` in the path would point
    /// somewhere else entirely once the prefix is added.
    #[test]
    fn a_path_with_dot_dot_is_never_made_verbatim() {
        assert_eq!(windows_verbatim(r"C:\a\..\b"), None);
        assert_eq!(windows_verbatim(r"C:\a\.\b"), None);
    }

    #[test]
    fn forward_slashes_are_normalised_before_the_prefix_goes_on() {
        assert_eq!(windows_verbatim("C:/a/b"), Some(r"\\?\C:\a\b".to_string()));
    }
}
