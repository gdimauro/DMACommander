//! Panel state: the cursor, the scroll window and the selection.
//!
//! This is pure state with no rendering in it. The TUI asks it for the visible
//! slice and draws that; the GPU backend asks the same question and gets the
//! same answer. A panel holding a million entries costs the renderer nothing
//! because [`Panel::visible`] is O(rows on screen).

use crate::entry::{Entry, SortKey, SortOrder, sort_entries};
use std::collections::HashSet;

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

/// A Shift+arrow selection in progress.
///
/// Anchored rather than toggle-and-move, so backing off shrinks the selection
/// instead of stamping a second toggle over it. This is also what the mouse
/// drag does, and two gestures that select a range should not disagree about
/// what happens when you reverse.
#[derive(Debug)]
struct SelectionGesture {
    /// Where the gesture began. The span is always anchor..=cursor.
    anchor: usize,
    /// The span as of the last extend, so shrinking knows what to give back.
    lo: usize,
    hi: usize,
    /// Rows inside the span the user had *already* marked before the gesture
    /// started. Backing off must leave those alone — they were not ours to clear.
    /// Recorded lazily as the span grows, so the cost is the size of the gesture
    /// and not the size of the directory.
    pre_marked: HashSet<usize>,
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
    /// `Some` only while Shift+arrows are being held down.
    gesture: Option<SelectionGesture>,
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
            gesture: None,
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

    /// Extend a Shift-selection to `target`, starting one if none is running.
    ///
    /// The span is always anchor..=target: moving back towards the anchor gives
    /// rows up rather than toggling them a second time.
    pub fn extend_selection_to(&mut self, target: usize) {
        if self.entries.is_empty() {
            return;
        }
        let target = target.min(self.entries.len() - 1);

        // Starting a gesture: the anchor is wherever the cursor already is, and
        // that row joins the selection immediately — Shift+Down from an unmarked
        // row should select both rows, not just the one you land on.
        if self.gesture.is_none() {
            let anchor = self.cursor;
            let mut g = SelectionGesture {
                anchor,
                lo: anchor,
                hi: anchor,
                pre_marked: HashSet::new(),
            };
            Self::remember_prior(&mut g, &self.entries, anchor);
            self.gesture = Some(g);
            self.mark(anchor, true);
        }

        let (old_lo, old_hi, anchor) = match &self.gesture {
            Some(g) => (g.lo, g.hi, g.anchor),
            None => return,
        };
        let (lo, hi) = if target <= anchor {
            (target, anchor)
        } else {
            (anchor, target)
        };

        // Rows leaving the span go back to how the user left them.
        for i in old_lo..=old_hi {
            if i < lo || i > hi {
                let was = self
                    .gesture
                    .as_ref()
                    .is_some_and(|g| g.pre_marked.contains(&i));
                self.mark(i, was);
            }
        }

        // Rows joining it get marked, remembering their prior state first.
        for i in lo..=hi {
            if i < old_lo || i > old_hi {
                if let Some(g) = self.gesture.as_mut() {
                    Self::remember_prior(g, &self.entries, i);
                }
                self.mark(i, true);
            }
        }

        if let Some(g) = self.gesture.as_mut() {
            g.lo = lo;
            g.hi = hi;
        }
        self.move_to(target);
    }

    /// Extend by a relative step, saturating at both ends.
    pub fn extend_selection_by(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let last = self.entries.len() - 1;
        let target = self.cursor.saturating_add_signed(delta).min(last);
        self.extend_selection_to(target);
    }

    /// Extend by whole screens, for Shift+PageUp / Shift+PageDown.
    pub fn extend_selection_page(&mut self, pages: isize) {
        self.extend_selection_by(pages * self.viewport as isize);
    }

    /// End the current Shift-selection, if any.
    ///
    /// Called by every plain movement, so a later Shift+arrow starts a fresh
    /// span from where the cursor now is rather than resuming an old anchor.
    pub fn end_selection_gesture(&mut self) {
        self.gesture = None;
    }

    /// Whether a Shift-selection is in progress — the status line says so, and
    /// a mode with no visible indicator is a mode people get stuck in.
    pub fn selecting(&self) -> bool {
        self.gesture.is_some()
    }

    /// How many rows the running gesture spans.
    pub fn gesture_len(&self) -> usize {
        self.gesture.as_ref().map_or(0, |g| g.hi - g.lo + 1)
    }

    fn remember_prior(g: &mut SelectionGesture, entries: &[Entry], i: usize) {
        if entries.get(i).is_some_and(|e| e.selected) {
            g.pre_marked.insert(i);
        }
    }

    /// Set or clear one row's mark. `..` is never markable.
    fn mark(&mut self, index: usize, on: bool) {
        if let Some(e) = self.entries.get_mut(index)
            && e.kind != crate::entry::EntryKind::Parent
        {
            e.selected = on;
        }
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

    /// Whether anything is ticked.
    ///
    /// Not the same question as "are there operands": with nothing marked, the
    /// operand is the row under the cursor, and there is always one of those.
    /// Esc has to know the difference — clearing a selection nobody made would
    /// mean Esc did nothing visible and then claimed it had.
    pub fn has_marks(&self) -> bool {
        self.entries.iter().any(|e| e.selected)
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

    // ---- Shift+arrow selection ----

    fn marked(p: &Panel) -> Vec<usize> {
        p.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.selected)
            .map(|(i, _)| i)
            .collect()
    }

    #[test]
    fn shift_down_selects_the_row_you_start_on_and_the_one_you_land_on() {
        let mut p = panel_with(10);
        p.move_to(1);
        p.extend_selection_by(1);
        assert_eq!(marked(&p), vec![1, 2]);
        assert_eq!(p.cursor(), 2);
    }

    #[test]
    fn extending_further_grows_the_span() {
        let mut p = panel_with(10);
        p.move_to(2);
        for _ in 0..3 {
            p.extend_selection_by(1);
        }
        assert_eq!(marked(&p), vec![2, 3, 4, 5]);
    }

    /// The whole reason for anchoring: reversing shrinks rather than toggling a
    /// second time.
    #[test]
    fn reversing_shrinks_the_selection_instead_of_toggling() {
        let mut p = panel_with(10);
        p.move_to(2);
        for _ in 0..3 {
            p.extend_selection_by(1);
        }
        p.extend_selection_by(-1);
        p.extend_selection_by(-1);
        assert_eq!(marked(&p), vec![2, 3]);
    }

    #[test]
    fn a_selection_can_run_upwards_from_the_anchor() {
        let mut p = panel_with(10);
        p.move_to(6);
        for _ in 0..2 {
            p.extend_selection_by(-1);
        }
        assert_eq!(marked(&p), vec![4, 5, 6]);
        assert_eq!(p.cursor(), 4);
    }

    #[test]
    fn crossing_back_over_the_anchor_reverses_the_direction() {
        let mut p = panel_with(10);
        p.move_to(5);
        p.extend_selection_by(2); // 5..7
        p.extend_selection_by(-4); // now 3..5
        assert_eq!(marked(&p), vec![3, 4, 5]);
    }

    /// Rows the user marked with Ins beforehand are not ours to clear when the
    /// span shrinks back over them.
    #[test]
    fn backing_off_preserves_marks_that_were_already_there() {
        let mut p = panel_with(10);
        p.move_to(4);
        p.toggle_selection(); // marks 4, steps to 5
        p.move_to(2);
        p.extend_selection_to(6); // sweeps across the pre-marked 4
        p.extend_selection_to(2); // back to just the anchor
        assert_eq!(
            marked(&p),
            vec![2, 4],
            "the pre-existing mark on 4 must survive"
        );
    }

    #[test]
    fn marks_outside_the_span_are_never_touched() {
        let mut p = panel_with(10);
        p.move_to(9);
        p.toggle_selection();
        p.move_to(1);
        p.extend_selection_by(2);
        assert!(
            p.entries[9].selected,
            "a mark far from the span must survive"
        );
    }

    #[test]
    fn the_parent_row_is_never_swept_into_a_selection() {
        let mut p = panel_with(10);
        p.move_to(2);
        p.extend_selection_to(0); // sweeps across `..`
        assert!(!p.entries[0].selected, "`..` must never be markable");
        assert_eq!(marked(&p), vec![1, 2]);
    }

    #[test]
    fn ending_the_gesture_makes_the_next_one_anchor_afresh() {
        let mut p = panel_with(10);
        p.move_to(2);
        p.extend_selection_by(2); // 2..4
        p.end_selection_gesture();
        p.move_to(7);
        p.extend_selection_by(1); // a new span at 7..8
        assert_eq!(marked(&p), vec![2, 3, 4, 7, 8]);
    }

    #[test]
    fn extending_saturates_at_both_ends() {
        let mut p = panel_with(5);
        p.move_to(3);
        p.extend_selection_by(100);
        assert_eq!(p.cursor(), 5);
        p.end_selection_gesture();
        p.move_to(3);
        p.extend_selection_by(-100);
        assert_eq!(p.cursor(), 0);
    }

    #[test]
    fn extending_in_an_empty_panel_does_nothing() {
        let mut p = Panel::new("/empty");
        p.set_entries(Vec::new());
        p.extend_selection_by(1);
        assert!(!p.selecting());
    }

    #[test]
    fn the_gesture_reports_its_own_size_for_the_status_line() {
        let mut p = panel_with(10);
        p.move_to(2);
        assert!(!p.selecting());
        p.extend_selection_by(3);
        assert!(p.selecting());
        assert_eq!(p.gesture_len(), 4);
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
