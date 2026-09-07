//! File viewer: what F3 shows, without knowing anything about a terminal.
//!
//! This crate sits below `dmac-tui` and holds no ratatui types on purpose. It
//! decides *what* the viewer shows — which lines, which bytes, where the
//! matches are — and the frontend decides how to paint it. That is what lets
//! the GPU backend show the same file from the same [`Document`] without a
//! second implementation of any of it.
//!
//! Three things here are opinions rather than mechanics, and each is a
//! judgement about what a viewer owes someone who pressed a key expecting to
//! see a file:
//!
//! - **It never reads a whole file.** A viewer that opens a 40GB log by loading
//!   it is a viewer that takes the machine down, and the person who pressed F3
//!   wanted to look at the top of it. There is a cap, and going over it is said
//!   out loud rather than hidden — [`Document::truncated`].
//! - **A bad byte does not refuse the file.** Invalid UTF-8 becomes U+FFFD.
//!   Refusing to show a log because one line came from a bad encoder is
//!   refusing to do the one thing that was asked.
//! - **Binary is shown as bytes, not as mojibake.** A file with a NUL in its
//!   first few kilobytes is not text, and pretending otherwise fills the screen
//!   with garbage and hides the one thing a hex view would have told you.

// Tests assert; `unwrap`/`expect` there are how a failure is reported.
// In non-test code the workspace lints still forbid them.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::io::Read;
use std::path::{Path, PathBuf};

/// How much of a file the viewer will hold at once.
///
/// Eight megabytes is far more text than anyone reads and small enough that a
/// dozen open documents are not a memory problem. Past it the document is
/// [`truncated`](Document::truncated) and says so, which is the honest answer:
/// the alternative is a viewer that appears to show a file and is quietly
/// showing part of one.
pub const READ_CAP: usize = 8 * 1024 * 1024;

/// How much of the head is examined to decide text or binary.
const SNIFF: usize = 8 * 1024;

/// Bytes on a row of the hex view. Sixteen, because that is what every hex dump
/// in existence uses and a viewer that chose differently would be a viewer you
/// have to think about.
pub const HEX_COLUMNS: usize = 16;

#[derive(Debug, thiserror::Error)]
pub enum ViewError {
    #[error("{path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{0} is a directory")]
    IsDirectory(String),
}

pub type Result<T> = std::result::Result<T, ViewError>;

/// What the file turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Binary,
}

/// A file open for looking at.
#[derive(Debug)]
pub struct Document {
    path: PathBuf,
    kind: Kind,
    /// The text, split into lines. Empty for a binary document.
    lines: Vec<String>,
    /// The raw bytes. Kept for a binary document, and for a text one so that
    /// switching to the hex view does not have to go back to the disk — the
    /// file may have changed since, and a hex view of a *different* file than
    /// the text view above it is worse than no hex view.
    bytes: Vec<u8>,
    /// Whether there was more file than [`READ_CAP`] allowed.
    truncated: bool,
    /// The file's full length, which is knowable even when the content is not.
    total_bytes: u64,
}

impl Document {
    /// Read `path`, as much of it as [`READ_CAP`] allows.
    pub fn open(path: &Path) -> Result<Self> {
        let io = |source: std::io::Error| ViewError::Io {
            path: path.display().to_string(),
            source,
        };
        let meta = std::fs::metadata(path).map_err(io)?;
        if meta.is_dir() {
            return Err(ViewError::IsDirectory(path.display().to_string()));
        }
        let total_bytes = meta.len();

        // `take` rather than `read_to_end`: the length in the metadata is a
        // claim about a moment that has already passed, and on /proc and
        // similar it is a lie by design — zero for a file with content. The cap
        // is enforced on what is actually read.
        let mut file = std::fs::File::open(path).map_err(io)?;
        let mut bytes = Vec::new();
        file.by_ref()
            .take(READ_CAP as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(io)?;
        let truncated = bytes.len() > READ_CAP;
        bytes.truncate(READ_CAP);

        let kind = match bytes.iter().take(SNIFF).any(|b| *b == 0) {
            true => Kind::Binary,
            false => Kind::Text,
        };
        let lines = match kind {
            Kind::Binary => Vec::new(),
            // Lossy on purpose: one bad byte must not cost you the file.
            Kind::Text => String::from_utf8_lossy(&bytes)
                .split('\n')
                .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
                .collect(),
        };

        Ok(Self {
            path: path.to_path_buf(),
            kind,
            lines,
            bytes,
            truncated,
            total_bytes,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }

    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// How many rows there are to scroll through, in the given view.
    ///
    /// The two views count differently — a line of text against sixteen bytes —
    /// and a scroll position that means one of them while the other is on
    /// screen is a scroll position that jumps when you toggle.
    pub fn rows(&self, hex: bool) -> usize {
        match hex || self.kind == Kind::Binary {
            true => self.bytes.len().div_ceil(HEX_COLUMNS),
            false => self.lines.len(),
        }
    }

    /// One row of the hex view: the offset, the bytes, and their printable
    /// forms.
    ///
    /// `None` past the end. The ASCII gutter shows a dot for anything outside
    /// printable ASCII rather than the byte's Unicode meaning: a hex dump whose
    /// right-hand column re-encodes is a hex dump that hides what the left-hand
    /// column is telling you.
    pub fn hex_row(&self, row: usize) -> Option<HexRow<'_>> {
        let from = row.checked_mul(HEX_COLUMNS)?;
        if from >= self.bytes.len() {
            return None;
        }
        let to = (from + HEX_COLUMNS).min(self.bytes.len());
        Some(HexRow {
            offset: from as u64,
            bytes: &self.bytes[from..to],
        })
    }

    /// Every row containing `needle`, in order.
    ///
    /// Case-insensitive, because a viewer's search is for finding something
    /// rather than for being precise about it — the precise one is `grep`, and
    /// it is a key away. Empty needle finds nothing rather than everything: a
    /// search that matches every line is a search that has lost your place.
    pub fn search(&self, needle: &str, hex: bool) -> Vec<usize> {
        if needle.is_empty() {
            return Vec::new();
        }
        let lower = needle.to_lowercase();
        if hex || self.kind == Kind::Binary {
            // In the hex view the text being searched is what is on screen:
            // the printable gutter. Searching the hex digits themselves would
            // match "de" in every second byte and find nothing anyone wanted.
            return (0..self.rows(true))
                .filter(|&r| {
                    self.hex_row(r)
                        .is_some_and(|h| h.printable().to_lowercase().contains(&lower))
                })
                .collect();
        }
        self.lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.to_lowercase().contains(&lower))
            .map(|(i, _)| i)
            .collect()
    }
}

/// One line of a hex dump.
#[derive(Debug, Clone, Copy)]
pub struct HexRow<'a> {
    pub offset: u64,
    pub bytes: &'a [u8],
}

impl HexRow<'_> {
    /// The bytes as hex pairs, padded to a full row so the gutter of a short
    /// last line still lines up with the ones above it.
    pub fn hex(&self) -> String {
        let mut out = String::with_capacity(HEX_COLUMNS * 3);
        for i in 0..HEX_COLUMNS {
            match self.bytes.get(i) {
                Some(b) => out.push_str(&format!("{b:02x} ")),
                None => out.push_str("   "),
            }
            // A gap down the middle, which is what makes a row of sixteen
            // countable by eye.
            if i == HEX_COLUMNS / 2 - 1 {
                out.push(' ');
            }
        }
        out
    }

    /// The printable gutter: ASCII as itself, everything else as a dot.
    pub fn printable(&self) -> String {
        self.bytes
            .iter()
            .map(|b| match b.is_ascii_graphic() || *b == b' ' {
                true => *b as char,
                false => '.',
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str, bytes: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dmac-view-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let p = dir.join(name);
        std::fs::write(&p, bytes).expect("write");
        p
    }

    #[test]
    fn a_text_file_becomes_lines_without_their_endings() {
        let p = scratch("text.txt", b"one\ntwo\r\nthree");
        let d = Document::open(&p).expect("open");
        assert_eq!(d.kind(), Kind::Text);
        assert_eq!(d.lines(), ["one", "two", "three"]);
        // A CRLF file must not show a stray carriage return at the end of every
        // line, which is what makes a Windows log unreadable in a naive viewer.
        assert!(d.lines().iter().all(|l| !l.contains('\r')));
    }

    /// One bad byte must not cost you the file. Refusing to show a log because
    /// a line came from a bad encoder is refusing the one thing asked for.
    #[test]
    fn invalid_utf8_is_shown_rather_than_refused() {
        let p = scratch("bad.txt", b"good\n\xff\xfe bad\nmore");
        let d = Document::open(&p).expect("open");
        assert_eq!(d.kind(), Kind::Text, "no NUL, so it is text");
        assert_eq!(d.lines().len(), 3);
        assert!(d.lines()[1].contains('\u{fffd}'), "{:?}", d.lines()[1]);
    }

    /// A NUL early on means it is not text, and showing it as text fills the
    /// screen with garbage while hiding what a hex view would have said.
    #[test]
    fn a_nul_makes_it_binary_and_it_is_shown_as_bytes() {
        let p = scratch("bin.dat", b"\x7fELF\x02\x01\x01\x00rest of it");
        let d = Document::open(&p).expect("open");
        assert_eq!(d.kind(), Kind::Binary);
        assert!(d.lines().is_empty(), "a binary file has no lines");

        let row = d.hex_row(0).expect("a first row");
        assert_eq!(row.offset, 0);
        assert!(row.hex().starts_with("7f 45 4c 46"), "{}", row.hex());
        assert_eq!(&row.printable()[..4], ".ELF");
    }

    /// The last row is short, and its gutter still has to line up with the
    /// rows above it or the dump is unreadable.
    #[test]
    fn a_short_last_row_is_padded_to_the_full_width() {
        let p = scratch("short.dat", b"\x00ab");
        let d = Document::open(&p).expect("open");
        let full = d.hex_row(0).expect("row").hex().len();
        let p2 = scratch("full.dat", &[0u8; HEX_COLUMNS]);
        let d2 = Document::open(&p2).expect("open");
        assert_eq!(full, d2.hex_row(0).expect("row").hex().len());
        assert!(d.hex_row(1).is_none(), "there is no second row");
    }

    /// A viewer that opens a 40GB log by loading it is a viewer that takes the
    /// machine down. There is a cap, and going over it is said out loud.
    #[test]
    fn a_file_past_the_cap_is_cut_and_says_so() {
        let big = vec![b'x'; READ_CAP + 1024];
        let p = scratch("big.txt", &big);
        let d = Document::open(&p).expect("open");
        assert!(d.truncated(), "it read the lot");
        assert_eq!(d.bytes().len(), READ_CAP);
        assert_eq!(
            d.total_bytes(),
            (READ_CAP + 1024) as u64,
            "the real length is knowable even when the content is not"
        );

        let small = scratch("small.txt", b"hello");
        assert!(!Document::open(&small).expect("open").truncated());
    }

    #[test]
    fn a_directory_is_refused_by_name_rather_than_by_an_io_error() {
        let dir = std::env::temp_dir().join(format!("dmac-view-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        assert!(matches!(
            Document::open(&dir),
            Err(ViewError::IsDirectory(_))
        ));
    }

    #[test]
    fn search_finds_rows_and_ignores_case() {
        let p = scratch("search.txt", b"alpha\nBETA\ngamma beta\ndelta");
        let d = Document::open(&p).expect("open");
        assert_eq!(d.search("beta", false), vec![1, 2]);
        assert_eq!(d.search("BeTa", false), vec![1, 2]);
        assert!(d.search("nothing", false).is_empty());
        // A search that matches every line is a search that has lost your place.
        assert!(d.search("", false).is_empty());
    }

    /// In the hex view what is searched is the gutter, not the hex digits:
    /// searching those would match "de" in every second byte and find nothing
    /// anyone wanted.
    #[test]
    fn a_hex_search_looks_at_the_printable_column() {
        let p = scratch(
            "hexsearch.dat",
            b"\x00\x00\x00\x00\x00\x00\x00\x00\
\x00\x00\x00\x00\x00\x00\x00\x00needle here",
        );
        let d = Document::open(&p).expect("open");
        assert_eq!(d.kind(), Kind::Binary);
        assert_eq!(d.search("needle", true), vec![1]);
        // "00" is on every row of the dump and on none of the gutters.
        assert!(d.search("00", true).is_empty());
    }

    /// The two views count rows differently, and a scroll position that means
    /// one of them while the other is showing is one that jumps on a toggle.
    #[test]
    fn the_two_views_count_their_own_rows() {
        let p = scratch("rows.txt", b"one\ntwo\nthree");
        let d = Document::open(&p).expect("open");
        assert_eq!(d.rows(false), 3);
        assert_eq!(d.rows(true), 13_usize.div_ceil(HEX_COLUMNS));
    }
}
