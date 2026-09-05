//! Panel state: the cursor, the scroll window and the selection.
//!
//! This is pure state with no rendering in it. The TUI asks it for the visible
//! slice and draws that; the GPU backend asks the same question and gets the
//! same answer. A panel holding a million entries costs the renderer nothing
//! because [`Panel::visible`] is O(rows on screen).

use crate::entry::{Entry, SortKey, SortOrder, sort_entries};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PanelId {
    Left,
    Right,
}

impl PanelId {
    pub fn other(self) -> Self {
        match self {
            PanelId::Left => PanelId::Right,
            PanelId::Right => PanelId::Left,
        }
    }
}

#[derive(Debug)]
pub struct Panel {
    /// Displayed as the panel title. A VFS location, not necessarily a real path.
    pub location: String,
    pub entries: Vec<Entry>,
    cursor: usize,
    /// Index of the first visible row. Kept in sync with `cursor` by [`Panel::scroll_to_cursor`].
    offset: usize,
    /// Rows the panel could show at the last draw. The renderer reports it back
    /// so paging keys work without the core knowing anything about terminals.
    viewport: usize,
    pub sort_key: SortKey,
    pub sort_order: SortOrder,
    pub show_hidden: bool,
}

impl Panel {
    pub fn new(location: impl Into<String>) -> Self {
        Self {
            location: location.into(),
            entries: Vec::new(),
            cursor: 0,
            offset: 0,
            viewport: 1,
            sort_key: SortKey::Name,
            sort_order: SortOrder::Ascending,
            show_hidden: false,
        }
    }

    /// Replace the listing, e.g. after entering a directory. Resets the cursor,
    /// because the old index means nothing in a new directory.
    pub fn set_entries(&mut self, entries: Vec<Entry>) {
        self.entries = entries;
        self.resort();
        self.cursor = 0;
        self.offset = 0;
    }

    pub fn resort(&mut self) {
        // Hold onto the highlighted name so the cursor follows the file the user
        // was looking at rather than jumping to whatever lands on that index.
        let anchor = self.current().map(|e| e.name.clone());
        sort_entries(&mut self.entries, self.sort_key, self.sort_order);
        if let Some(name) = anchor
            && let Some(i) = self.entries.iter().position(|e| e.name == name)
        {
            self.cursor = i;
        }
        self.clamp();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn offset(&self) -> usize {
        self.offset
    }

    pub fn current(&self) -> Option<&Entry> {
        self.entries.get(self.cursor)
    }

    /// Told by the renderer how many rows fit. Also re-clamps the scroll window,
    /// so a terminal resize cannot leave the cursor off screen.
    pub fn set_viewport(&mut self, rows: usize) {
        self.viewport = rows.max(1);
        self.scroll_to_cursor();
    }

    /// The rows the renderer should draw, and the index of the first one.
    pub fn visible(&self) -> (usize, &[Entry]) {
        let start = self.offset.min(self.entries.len());
        let end = (start + self.viewport).min(self.entries.len());
        (start, &self.entries[start..end])
    }

    pub fn move_cursor(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let last = self.entries.len() - 1;
        self.cursor = self.cursor.saturating_add_signed(delta).min(last);
        self.scroll_to_cursor();
    }

    pub fn move_to(&mut self, index: usize) {
        self.cursor = index.min(self.entries.len().saturating_sub(1));
        self.scroll_to_cursor();
    }

    pub fn page(&mut self, pages: isize) {
        self.move_cursor(pages * self.viewport as isize);
    }

    pub fn go_home(&mut self) {
        self.move_to(0);
    }

    pub fn go_end(&mut self) {
        self.move_to(self.entries.len().saturating_sub(1));
    }

    /// Ins: mark the row and step down, so holding Ins sweeps a selection.
    /// `..` is never selectable — selecting the parent is how people accidentally
    /// delete the directory they are standing in.
    pub fn toggle_selection(&mut self) {
        if let Some(e) = self.entries.get_mut(self.cursor)
            && !e.is_dir_like_parent()
        {
            e.selected = !e.selected;
        }
        self.move_cursor(1);
    }

    pub fn invert_selection(&mut self) {
        for e in self.entries.iter_mut().filter(|e| !e.is_dir_like_parent()) {
            e.selected = !e.selected;
        }
    }

    pub fn clear_selection(&mut self) {
        for e in &mut self.entries {
            e.selected = false;
        }
    }

    /// What an operation acts on: the marked rows, or — when nothing is marked —
    /// the row under the cursor. This fallback is the orthodox contract; without
    /// it every single-file copy needs an extra keystroke.
    pub fn operands(&self) -> Vec<&Entry> {
        let marked: Vec<&Entry> = self.entries.iter().filter(|e| e.selected).collect();
        if marked.is_empty() {
            self.current()
                .filter(|e| !e.is_dir_like_parent())
                .into_iter()
                .collect()
        } else {
            marked
        }
    }

    /// Alt+letter incremental search: jump to the next entry whose name starts
    /// with `prefix`, wrapping around from the current position.
    pub fn find_prefix(&mut self, prefix: &str) -> bool {
        if prefix.is_empty() || self.entries.is_empty() {
            return false;
        }
        let lower = prefix.to_lowercase();
        let n = self.entries.len();
        for step in 1..=n {
            let i = (self.cursor + step) % n;
            if self.entries[i].name.to_lowercase().starts_with(&lower) {
                self.move_to(i);
                return true;
            }
        }
        false
    }

    fn scroll_to_cursor(&mut self) {
        if self.cursor < self.offset {
            self.offset = self.cursor;
        } else if self.cursor >= self.offset + self.viewport {
            self.offset = self.cursor + 1 - self.viewport;
        }
        self.clamp();
    }

    fn clamp(&mut self) {
        let last = self.entries.len().saturating_sub(1);
        self.cursor = self.cursor.min(last);
        let max_offset = self.entries.len().saturating_sub(self.viewport);
        self.offset = self.offset.min(max_offset);
    }
}

impl Entry {
    fn is_dir_like_parent(&self) -> bool {
        self.kind == crate::entry::EntryKind::Parent
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{Entry, EntryKind};

    fn panel_with(n: usize) -> Panel {
        let mut p = Panel::new("/test");
        let mut v = vec![Entry::parent()];
        v.extend((0..n).map(|i| Entry {
            name: format!("file{i:04}"),
            kind: EntryKind::File,
            size: Some(i as u64),
            modified: None,
            mode: None,
            selected: false,
        }));
        p.set_entries(v);
        p.set_viewport(10);
        p
    }

    #[test]
    fn visible_slice_is_viewport_sized_not_entry_sized() {
        let p = panel_with(100_000);
        let (start, rows) = p.visible();
        assert_eq!(start, 0);
        assert_eq!(rows.len(), 10);
    }

    #[test]
    fn cursor_never_leaves_the_viewport() {
        let mut p = panel_with(1000);
        p.move_cursor(500);
        let (start, rows) = p.visible();
        assert!((start..start + rows.len()).contains(&p.cursor()));
    }

    #[test]
    fn cursor_saturates_at_both_ends() {
        let mut p = panel_with(5);
        p.move_cursor(-100);
        assert_eq!(p.cursor(), 0);
        p.move_cursor(100);
        assert_eq!(p.cursor(), 5); // 5 files + `..`
    }

    #[test]
    fn parent_cannot_be_selected() {
        let mut p = panel_with(3);
        p.move_to(0);
        p.toggle_selection();
        assert!(!p.entries[0].selected, "`..` must never be markable");
    }

    #[test]
    fn operands_fall_back_to_the_cursor_row() {
        let mut p = panel_with(3);
        p.move_to(2);
        assert_eq!(p.operands().len(), 1);
        p.toggle_selection();
        p.move_to(3);
        p.toggle_selection();
        assert_eq!(p.operands().len(), 2);
    }

    #[test]
    fn operands_are_empty_when_only_parent_is_under_the_cursor() {
        let p = panel_with(0);
        assert!(p.operands().is_empty());
    }

    #[test]
    fn resort_keeps_the_cursor_on_the_same_file() {
        let mut p = panel_with(20);
        p.move_to(3);
        let name = p.current().unwrap().name.clone();
        p.sort_order = SortOrder::Descending;
        p.resort();
        assert_eq!(p.current().unwrap().name, name);
    }

    #[test]
    fn prefix_search_wraps_around() {
        let mut p = panel_with(20);
        p.move_to(19);
        assert!(p.find_prefix("file0001"));
        assert_eq!(p.current().unwrap().name, "file0001");
    }

    #[test]
    fn shrinking_the_viewport_keeps_the_cursor_visible() {
        let mut p = panel_with(100);
        p.move_to(50);
        p.set_viewport(3);
        let (start, rows) = p.visible();
        assert!((start..start + rows.len()).contains(&p.cursor()));
    }
}
