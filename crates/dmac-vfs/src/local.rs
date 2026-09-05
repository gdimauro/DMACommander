//! The local filesystem backend.
//!
//! The one backend that must never be slow and never be wrong, because it is
//! what 95% of sessions use. It streams entries in chunks so a directory with a
//! million files paints its first screen immediately.

use crate::{Capabilities, ListChunk, Result, VfsBackend, VfsError, VfsPath};
use dmac_core::{Entry, EntryKind};
use std::path::Path;
use tokio::sync::mpsc::Sender;

/// How many entries accumulate before a chunk is sent. Small enough that the
/// first screen appears instantly, large enough that a million-entry directory
/// does not drown the channel in wakeups.
const CHUNK: usize = 256;

#[derive(Debug, Default, Clone)]
pub struct LocalBackend;

impl LocalBackend {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl VfsBackend for LocalBackend {
    fn name(&self) -> &'static str {
        "local"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            read: true,
            write: true,
            rename: true,
            delete: true,
            create_dir: true,
            symlinks: cfg!(unix),
            unix_permissions: cfg!(unix),
            random_write: true,
        }
    }

    async fn list(&self, path: &VfsPath, tx: Sender<Result<ListChunk>>) -> Result<()> {
        let dir = path.as_path().to_path_buf();
        let mut rd = tokio::fs::read_dir(&dir)
            .await
            .map_err(|e| map_io(&dir, e))?;

        // `..` first, and only when there is somewhere to go. At the filesystem
        // root the row would be a dead end, so we omit it.
        let mut buf = Vec::with_capacity(CHUNK);
        if path.parent().is_some() {
            buf.push(Entry::parent());
        }

        loop {
            // `next_entry` is the cancellation point: dropping this future while
            // the panel navigates away stops the walk instead of finishing it.
            let next = rd.next_entry().await.map_err(|e| map_io(&dir, e))?;
            let Some(dent) = next else { break };

            buf.push(entry_from_dirent(&dent).await);

            if buf.len() >= CHUNK {
                let chunk = ListChunk {
                    entries: std::mem::take(&mut buf),
                    complete: false,
                };
                // A closed receiver means the panel moved on. That is normal, not
                // an error — stop quietly rather than logging noise.
                if tx.send(Ok(chunk)).await.is_err() {
                    return Ok(());
                }
                buf = Vec::with_capacity(CHUNK);
            }
        }

        let _ = tx
            .send(Ok(ListChunk {
                entries: buf,
                complete: true,
            }))
            .await;
        Ok(())
    }

    async fn stat(&self, path: &VfsPath) -> Result<Entry> {
        let p = path.as_path();
        let meta = tokio::fs::symlink_metadata(p)
            .await
            .map_err(|e| map_io(p, e))?;
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.to_string_lossy().into_owned());
        Ok(entry_from_meta(name, &meta))
    }

    async fn read(&self, path: &VfsPath, max_bytes: u64) -> Result<Vec<u8>> {
        use tokio::io::AsyncReadExt;

        let p = path.as_path();
        let file = tokio::fs::File::open(p).await.map_err(|e| map_io(p, e))?;
        let mut buf = Vec::new();
        // `take` is the guard that keeps a preview of /dev/zero from eating all
        // available memory. Never read an unbounded file into a Vec.
        file.take(max_bytes)
            .read_to_end(&mut buf)
            .await
            .map_err(|e| map_io(p, e))?;
        Ok(buf)
    }
}

/// Build an entry from a `DirEntry`, preferring the cheap `file_type()` that most
/// platforms answer straight from the directory block without a second syscall.
async fn entry_from_dirent(dent: &tokio::fs::DirEntry) -> Entry {
    let name = dent.file_name().to_string_lossy().into_owned();

    let kind = match dent.file_type().await {
        Ok(ft) if ft.is_dir() => EntryKind::Dir,
        Ok(ft) if ft.is_symlink() => EntryKind::Symlink,
        Ok(ft) if ft.is_file() => EntryKind::File,
        Ok(_) => EntryKind::Other,
        // A file deleted between readdir and stat is routine, not exceptional.
        Err(_) => EntryKind::Other,
    };

    match dent.metadata().await {
        Ok(meta) => {
            let mut e = entry_from_meta(name, &meta);
            // `metadata()` follows symlinks; keep the link's own kind so the panel
            // shows it as a link rather than as its target.
            if kind == EntryKind::Symlink {
                e.kind = EntryKind::Symlink;
            }
            e
        }
        Err(_) => Entry {
            name,
            kind,
            size: None,
            modified: None,
            mode: None,
            selected: false,
        },
    }
}

fn entry_from_meta(name: String, meta: &std::fs::Metadata) -> Entry {
    let kind = if meta.is_dir() {
        EntryKind::Dir
    } else if meta.is_symlink() {
        EntryKind::Symlink
    } else if meta.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    };

    Entry {
        name,
        kind,
        // A directory's byte size is meaningless to a user; the panel shows
        // "<DIR>" there instead, so do not invent a number.
        size: (kind != EntryKind::Dir).then_some(meta.len()),
        modified: meta.modified().ok(),
        mode: unix_mode(meta),
        selected: false,
    }
}

#[cfg(unix)]
fn unix_mode(meta: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(meta.mode())
}

#[cfg(not(unix))]
fn unix_mode(_meta: &std::fs::Metadata) -> Option<u32> {
    None
}

fn map_io(path: &Path, e: std::io::Error) -> VfsError {
    let p = path.to_string_lossy().into_owned();
    match e.kind() {
        std::io::ErrorKind::NotFound => VfsError::NotFound { path: p },
        std::io::ErrorKind::PermissionDenied => VfsError::PermissionDenied { path: p },
        _ => VfsError::Io { path: p, source: e },
    }
}

/// Collect a whole listing. Convenience for callers that genuinely want it all
/// (tests, scripting); the panel uses the streaming API instead.
pub async fn list_all(backend: &dyn VfsBackend, path: &VfsPath) -> Result<Vec<Entry>> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let walk = backend.list(path, tx);
    let collect = async {
        let mut out = Vec::new();
        while let Some(chunk) = rx.recv().await {
            out.extend(chunk?.entries);
        }
        Ok(out)
    };
    let (walk_result, entries) = tokio::join!(walk, collect);
    walk_result?;
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Every filesystem test owns a TempDir. A test that writes outside one is a
    /// defect — see `.claude/agents/qa-engineer.md`.
    fn tree(files: &[&str], dirs: &[&str]) -> tempfile::TempDir {
        let td = tempfile::tempdir().expect("tempdir");
        for d in dirs {
            std::fs::create_dir_all(td.path().join(d)).expect("mkdir");
        }
        for f in files {
            let mut fh = std::fs::File::create(td.path().join(f)).expect("create");
            writeln!(fh, "contents of {f}").expect("write");
        }
        td
    }

    #[tokio::test]
    async fn lists_files_and_directories() {
        let td = tree(&["a.txt", "b.rs"], &["sub"]);
        let entries = list_all(&LocalBackend::new(), &VfsPath::local(td.path()))
            .await
            .expect("list");

        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&".."));
        assert!(names.contains(&"a.txt"));
        assert!(names.contains(&"sub"));

        let sub = entries.iter().find(|e| e.name == "sub").unwrap();
        assert_eq!(sub.kind, EntryKind::Dir);
        // A directory's byte size is meaningless; we must not invent one.
        assert_eq!(sub.size, None);

        let a = entries.iter().find(|e| e.name == "a.txt").unwrap();
        assert_eq!(a.kind, EntryKind::File);
        assert!(a.size.unwrap() > 0);
    }

    #[tokio::test]
    async fn streams_in_chunks_rather_than_one_giant_batch() {
        // More than one CHUNK, so the panel gets a first screen before the walk
        // finishes — the property that makes huge directories feel instant.
        let names: Vec<String> = (0..CHUNK * 2 + 7).map(|i| format!("f{i:05}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let td = tree(&refs, &[]);

        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let backend = LocalBackend::new();
        let path = VfsPath::local(td.path());

        let (walk, chunks) = tokio::join!(backend.list(&path, tx), async {
            let mut n = 0usize;
            let mut total = 0usize;
            while let Some(c) = rx.recv().await {
                let c = c.expect("chunk");
                total += c.entries.len();
                n += 1;
            }
            (n, total)
        });
        walk.expect("walk");

        let (chunk_count, total) = chunks;
        assert!(
            chunk_count >= 3,
            "expected streaming, got {chunk_count} chunk(s)"
        );
        assert_eq!(total, names.len() + 1, "all entries plus `..`");
    }

    #[tokio::test]
    async fn missing_directory_reports_not_found_not_a_panic() {
        let td = tempfile::tempdir().unwrap();
        let missing = VfsPath::local(td.path().join("nope"));
        let err = list_all(&LocalBackend::new(), &missing).await.unwrap_err();
        assert!(matches!(err, VfsError::NotFound { .. }), "got {err:?}");
    }

    #[tokio::test]
    async fn read_is_bounded_so_a_huge_file_cannot_exhaust_memory() {
        let td = tree(&["big.bin"], &[]);
        let path = VfsPath::local(td.path().join("big.bin"));
        let data = LocalBackend::new().read(&path, 4).await.expect("read");
        assert_eq!(data.len(), 4, "max_bytes must be honoured");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlink_is_reported_as_a_link_not_as_its_target() {
        let td = tree(&["real.txt"], &[]);
        std::os::unix::fs::symlink(td.path().join("real.txt"), td.path().join("link.txt"))
            .expect("symlink");

        let entries = list_all(&LocalBackend::new(), &VfsPath::local(td.path()))
            .await
            .expect("list");
        let link = entries.iter().find(|e| e.name == "link.txt").unwrap();
        assert_eq!(link.kind, EntryKind::Symlink);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_non_utf8_filename_does_not_crash_the_listing() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let td = tempfile::tempdir().unwrap();
        // 0xFF is not valid UTF-8. A file manager that panics here is not a
        // file manager.
        let raw = OsStr::from_bytes(b"bad\xFFname");
        // APFS (and any filesystem mounted with UTF-8 enforcement) refuses to
        // create this name at all. That is the filesystem protecting us, not a
        // pass — skip rather than assert a property we could not exercise.
        if std::fs::File::create(td.path().join(raw)).is_err() {
            eprintln!("skipped: this filesystem enforces UTF-8 filenames");
            return;
        }

        let entries = list_all(&LocalBackend::new(), &VfsPath::local(td.path()))
            .await
            .expect("listing must survive a non-UTF-8 name");
        assert!(entries.iter().any(|e| e.name.contains('\u{FFFD}')));
    }

    #[tokio::test]
    async fn dropping_the_receiver_stops_the_walk_instead_of_erroring() {
        let names: Vec<String> = (0..CHUNK * 3).map(|i| format!("f{i:05}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let td = tree(&refs, &[]);

        let (tx, rx) = tokio::sync::mpsc::channel(1);
        drop(rx); // the panel navigated away mid-walk
        let result = LocalBackend::new()
            .list(&VfsPath::local(td.path()), tx)
            .await;
        assert!(
            result.is_ok(),
            "an abandoned listing is normal, not an error"
        );
    }
}
