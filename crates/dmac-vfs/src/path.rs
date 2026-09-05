//! Paths that survive being on the wrong operating system.
//!
//! A path is never just a `String` here. It carries the scheme that owns it, and
//! it never assumes UTF-8 validity is optional to think about: on Unix a filename
//! is bytes, and a file manager that panics on one is not a file manager.

use std::fmt;
use std::path::{Path, PathBuf};

/// Which backend owns a path. Nesting is expressed by the `inner` chain, so
/// `sftp://host/a.zip/dir` is `Sftp` wrapping `Archive`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scheme {
    Local,
    Archive,
    Sftp,
    S3,
    Ftp,
    WebDav,
    /// Anything reached through OpenDAL that we have not given a first-class name.
    OpenDal(String),
}

impl fmt::Display for Scheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Scheme::Local => f.write_str("file"),
            Scheme::Archive => f.write_str("arc"),
            Scheme::Sftp => f.write_str("sftp"),
            Scheme::S3 => f.write_str("s3"),
            Scheme::Ftp => f.write_str("ftp"),
            Scheme::WebDav => f.write_str("dav"),
            Scheme::OpenDal(s) => f.write_str(s),
        }
    }
}

/// A location inside some backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VfsPath {
    pub scheme: Scheme,
    /// For `Local`, a real OS path. For remote schemes, an absolute POSIX-style
    /// path within that backend — never an OS path, so Windows separators never
    /// leak into an S3 key.
    inner: PathBuf,
}

impl VfsPath {
    pub fn local(path: impl Into<PathBuf>) -> Self {
        Self {
            scheme: Scheme::Local,
            inner: path.into(),
        }
    }

    pub fn as_path(&self) -> &Path {
        &self.inner
    }

    pub fn is_local(&self) -> bool {
        self.scheme == Scheme::Local
    }

    /// Descend into a child. Rejects anything that would escape the current
    /// directory: this is the chokepoint that stops zip-slip and `..` traversal
    /// coming from a hostile archive or a malicious server listing.
    pub fn join(&self, component: &str) -> Option<Self> {
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.contains('/')
            || component.contains('\\')
            || component.contains('\0')
        {
            return None;
        }
        Some(Self {
            scheme: self.scheme.clone(),
            inner: self.inner.join(component),
        })
    }

    /// Go up one level. `None` at the root, which is how the panel knows not to
    /// show a `..` row.
    ///
    /// A relative path with a single component has an *empty* parent according
    /// to `Path`, and an empty path is not a directory anyone can list. Treating
    /// that as "no parent" is what stops `..` from navigating into nothing —
    /// which it did, leaving a blank panel with no way back.
    pub fn parent(&self) -> Option<Self> {
        let parent = self.inner.parent()?;
        if parent.as_os_str().is_empty() {
            return None;
        }
        Some(Self {
            scheme: self.scheme.clone(),
            inner: parent.to_path_buf(),
        })
    }

    /// What the panel title shows. Lossy on purpose: a non-UTF-8 filename is
    /// displayed with replacement characters rather than crashing the renderer.
    pub fn display(&self) -> String {
        match self.scheme {
            Scheme::Local => self.inner.to_string_lossy().into_owned(),
            _ => format!("{}://{}", self.scheme, self.inner.to_string_lossy()),
        }
    }
}

impl fmt::Display for VfsPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_refuses_traversal() {
        let root = VfsPath::local("/home/user");
        for hostile in ["..", "../etc", "a/b", "a\\b", ".", "", "a\0b"] {
            assert!(
                root.join(hostile).is_none(),
                "join must reject {hostile:?} — this is the zip-slip chokepoint"
            );
        }
    }

    #[test]
    fn join_accepts_ordinary_names() {
        let root = VfsPath::local("/home/user");
        let child = root.join("Documents").expect("plain name must be accepted");
        assert_eq!(child.as_path(), Path::new("/home/user/Documents"));
    }

    #[test]
    fn join_accepts_names_that_merely_look_scary() {
        let root = VfsPath::local("/tmp");
        assert!(root.join("..hidden").is_some());
        assert!(root.join("file..txt").is_some());
    }

    #[test]
    fn parent_stops_at_the_root() {
        let mut p = VfsPath::local("/a/b");
        p = p.parent().unwrap();
        assert_eq!(p.as_path(), Path::new("/a"));
        p = p.parent().unwrap();
        assert_eq!(p.as_path(), Path::new("/"));
        assert!(p.parent().is_none(), "root has no parent");
    }

    /// `Path::parent` of a one-component relative path is the empty path, which
    /// is not a directory. Navigating there left a blank panel with no way out.
    #[test]
    fn a_relative_path_does_not_have_an_empty_parent() {
        assert!(VfsPath::local("docs").parent().is_none());
        assert!(VfsPath::local("a").parent().is_none());
        let nested = VfsPath::local("a/b");
        assert_eq!(nested.parent().unwrap().as_path(), Path::new("a"));
    }

    #[test]
    fn remote_paths_show_their_scheme() {
        let p = VfsPath {
            scheme: Scheme::S3,
            inner: PathBuf::from("/bucket/key"),
        };
        assert_eq!(p.display(), "s3:///bucket/key");
    }
}
