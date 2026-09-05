//! A single row in a panel, whatever filesystem it came from.

use std::time::SystemTime;

/// What a panel row actually is. Kept coarse on purpose: a backend that cannot
/// distinguish a socket from a fifo reports [`EntryKind::Other`] rather than lying.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum EntryKind {
    /// The `..` row. Always sorts first, is never selectable, is never deleted.
    Parent,
    Dir,
    File,
    /// A symlink, plus what it resolves to (if the backend could resolve it).
    Symlink,
    Other,
}

impl EntryKind {
    /// Directories and `..` group above files in every orthodox file manager.
    /// This ordering is muscle memory; do not make it configurable away.
    fn sort_group(self) -> u8 {
        match self {
            EntryKind::Parent => 0,
            EntryKind::Dir => 1,
            EntryKind::Symlink | EntryKind::File | EntryKind::Other => 2,
        }
    }
}

/// One row. Deliberately cheap to clone: a panel holds hundreds of thousands.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
    /// `None` when the backend has not stat'ed it yet — listings stream in, so
    /// a row can exist before its metadata arrives. Render this as `?`, never `0`.
    pub size: Option<u64>,
    pub modified: Option<SystemTime>,
    /// Unix mode bits, when the backend has them. Windows backends leave it `None`.
    pub mode: Option<u32>,
    /// The panel's own mark (Ins / Gray+). Never persisted to disk by the backend.
    pub selected: bool,
}

impl Entry {
    pub fn parent() -> Self {
        Self {
            name: "..".into(),
            kind: EntryKind::Parent,
            size: None,
            modified: None,
            mode: None,
            selected: false,
        }
    }

    pub fn is_dir_like(&self) -> bool {
        matches!(self.kind, EntryKind::Dir | EntryKind::Parent)
    }

    /// The extension used for sorting and colouring. Dotfiles have no extension:
    /// `.gitignore` is a name, not an extension, and sorting it under "gitignore"
    /// is the kind of detail that makes the app feel wrong.
    pub fn extension(&self) -> &str {
        match self.name.rfind('.') {
            Some(0) | None => "",
            Some(i) => &self.name[i + 1..],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SortKey {
    Name,
    Extension,
    Size,
    Modified,
    /// Whatever order the backend handed us. The only zero-cost option, and the
    /// right default for a directory with a million entries.
    Unsorted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SortOrder {
    Ascending,
    Descending,
}

/// Sort in place, always keeping `..` first and directories above files.
pub fn sort_entries(entries: &mut [Entry], key: SortKey, order: SortOrder) {
    if key == SortKey::Unsorted {
        // Even unsorted, `..` and directories keep their groups.
        entries.sort_by_key(|e| e.kind.sort_group());
        return;
    }

    entries.sort_by(|a, b| {
        a.kind.sort_group().cmp(&b.kind.sort_group()).then_with(|| {
            let ord = match key {
                SortKey::Name => natural_cmp(&a.name, &b.name),
                SortKey::Extension => natural_cmp(a.extension(), b.extension())
                    .then_with(|| natural_cmp(&a.name, &b.name)),
                SortKey::Size => a.size.cmp(&b.size),
                SortKey::Modified => a.modified.cmp(&b.modified),
                SortKey::Unsorted => std::cmp::Ordering::Equal,
            };
            match order {
                SortOrder::Ascending => ord,
                SortOrder::Descending => ord.reverse(),
            }
        })
    });
}

/// Case-insensitive comparison that orders embedded digit runs numerically, so
/// `file2` comes before `file10`. Users read filenames, not byte sequences.
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let mut ai = a.char_indices().peekable();
    let mut bi = b.char_indices().peekable();

    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some((apos, ac)), Some((bpos, bc))) => {
                if ac.is_ascii_digit() && bc.is_ascii_digit() {
                    let an = take_digits(a, apos, &mut ai);
                    let bn = take_digits(b, bpos, &mut bi);
                    // Compare by length first: avoids overflow on absurdly long
                    // digit runs, which a hostile filename can absolutely contain.
                    match an.len().cmp(&bn.len()).then_with(|| an.cmp(bn)) {
                        Ordering::Equal => continue,
                        other => return other,
                    }
                }

                let (al, bl) = (ac.to_ascii_lowercase(), bc.to_ascii_lowercase());
                if al != bl {
                    return al.cmp(&bl);
                }
                ai.next();
                bi.next();
            }
        }
    }
}

/// Consume the digit run starting at `start`, returning it with leading zeros
/// stripped so `007` and `7` compare equal.
fn take_digits<'a, I>(s: &'a str, start: usize, it: &mut std::iter::Peekable<I>) -> &'a str
where
    I: Iterator<Item = (usize, char)>,
{
    let mut end = start;
    while let Some(&(pos, c)) = it.peek() {
        if !c.is_ascii_digit() {
            break;
        }
        end = pos + c.len_utf8();
        it.next();
    }
    s[start..end].trim_start_matches('0')
}

/// Format a byte count the way an orthodox file manager does: exact digits with
/// thousands separators up to 4 digits, then a compact unit. Users compare sizes
/// by eye, so the column must stay narrow and right-aligned.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "K", "M", "G", "T", "P"];
    if bytes < 1000 {
        return bytes.to_string();
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if value < 10.0 {
        format!("{value:.1}{}", UNITS[unit])
    } else {
        format!("{value:.0}{}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order_beats_lexicographic() {
        let mut names = ["file10", "file2", "File1"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, ["File1", "file2", "file10"]);
    }

    #[test]
    fn leading_zeros_do_not_change_order() {
        assert_eq!(natural_cmp("v007", "v7"), std::cmp::Ordering::Equal);
        assert_eq!(natural_cmp("v007", "v8"), std::cmp::Ordering::Less);
    }

    #[test]
    fn dotfiles_have_no_extension() {
        let e = Entry {
            name: ".gitignore".into(),
            kind: EntryKind::File,
            size: None,
            modified: None,
            mode: None,
            selected: false,
        };
        assert_eq!(e.extension(), "");
    }

    #[test]
    fn parent_always_sorts_first() {
        let mk = |name: &str, kind| Entry {
            name: name.into(),
            kind,
            size: None,
            modified: None,
            mode: None,
            selected: false,
        };
        let mut v = vec![
            mk("zebra.txt", EntryKind::File),
            mk("..", EntryKind::Parent),
            mk("alpha", EntryKind::Dir),
        ];
        sort_entries(&mut v, SortKey::Name, SortOrder::Descending);
        assert_eq!(v[0].name, "..");
        assert_eq!(v[1].name, "alpha");
    }

    #[test]
    fn size_column_stays_narrow() {
        assert_eq!(format_size(0), "0");
        assert_eq!(format_size(999), "999");
        assert_eq!(format_size(1024), "1.0K");
        assert_eq!(format_size(1024 * 1024 * 3), "3.0M");
        assert!(format_size(u64::MAX).len() <= 6);
    }
}
