//! Application state and the event loop.
//!
//! The loop does exactly three things: drain input, drain state updates, draw.
//! Everything expensive happens in a Tokio task and arrives here as a message —
//! that is the rule that keeps the UI responsive while 100k files are copying.

use crate::action::Action;
use crate::terminal::TerminalGuard;
use crate::theme::Theme;
use crate::{keymap, ui};
use dmac_core::{Panel, PanelId, SortOrder};
use dmac_fx::{Canvas, EffectKey, Screensaver, ScreensaverConfig, Wake};
use dmac_vfs::{BackendRef, ListChunk, VfsPath, local::LocalBackend};
use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use std::sync::Arc;
use tokio::sync::mpsc;

/// A state change produced off the UI thread.
enum Update {
    /// A slice of a directory listing arrived.
    Entries {
        panel: PanelId,
        /// Discarded if it does not match the panel's current generation — the
        /// user may have navigated away while the walk was in flight.
        generation: u64,
        chunk: ListChunk,
    },
    Error {
        panel: PanelId,
        message: String,
    },
}

/// Where the keyboard is. Exactly two places, never ambiguous.
///
/// This is deliberately explicit rather than the orthodox "typing always falls
/// through to the command line": with focus modelled, a letter typed on a panel
/// can mean incremental search, and the cursor can be shown only where text is
/// actually being edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Panel,
    CommandLine,
}

/// Which overlay, if any, owns the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Normal,
    /// The screensaver picker, with the highlighted row.
    Picker {
        selected: usize,
    },
    /// Contextual commands for the entry under the cursor.
    Context {
        selected: usize,
        anchor: (u16, u16),
    },
}

/// A left-button drag in progress: the row it started on, so the swept range is
/// recomputed from scratch on every move rather than accumulated — a fast drag
/// skips rows, and accumulating would leave holes in the selection.
#[derive(Debug, Clone, Copy)]
struct Drag {
    panel: PanelId,
    anchor: usize,
    /// `true` for a right-button sweep.
    toggling: bool,
}

/// Where things were drawn last frame, so a click maps back to a row. Rebuilt
/// on every draw, so a resize can never leave it stale.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct LayoutCache {
    /// Interior area of each panel — borders excluded, so row 0 is the first entry.
    pub panels: [Rect; 2],
    pub fkeys: Rect,
    pub command: Rect,
    /// Interior of the context menu while it is open.
    pub menu: Rect,
    /// Interior of the screensaver picker while it is open.
    pub picker: Rect,
}

pub struct App {
    pub(crate) panels: [Panel; 2],
    cwd: [VfsPath; 2],
    /// Bumped on every navigation so results from an abandoned listing are dropped.
    generation: [u64; 2],
    pub(crate) active: PanelId,
    pub(crate) theme: Theme,
    pub(crate) command_line: String,
    pub(crate) status: String,
    pub(crate) panels_hidden: bool,
    backend: BackendRef,
    screensaver: Screensaver,
    pub(crate) focus: Focus,
    pub(crate) mode: Mode,
    pub(crate) layout: LayoutCache,
    /// Incremental-search buffer, filled while a panel has focus. Cleared after
    /// a pause so an unrelated later keystroke does not extend an old search.
    quick_search: String,
    last_search: std::time::Instant,
    drag: Option<Drag>,
    /// Last left-click, for double-click detection.
    last_click: Option<(std::time::Instant, PanelId, usize)>,
    /// Whether the current right-button press has turned into a drag. A press
    /// that never moves is a click and opens the context menu; one that moves is
    /// a selection sweep. Distinguishing them is what lets the right button do
    /// both without a modifier.
    right_dragged: bool,
    /// Where the pointer is, so it can be drawn the way DOS text mode did.
    pub(crate) mouse: Option<(u16, u16)>,
    /// When the splash stops showing. `None` means it was never shown or has
    /// already gone, and contributes no timer either way.
    splash_until: Option<std::time::Instant>,
    should_quit: bool,
    tx: mpsc::UnboundedSender<Update>,
}

impl App {
    /// Private: `Update` is an internal message type, so the only supported
    /// entry point is [`run`].
    fn new(
        left: VfsPath,
        right: VfsPath,
        screensaver: ScreensaverConfig,
        splash: bool,
        tx: mpsc::UnboundedSender<Update>,
    ) -> Self {
        Self {
            panels: [Panel::new(left.display()), Panel::new(right.display())],
            cwd: [left, right],
            generation: [0, 0],
            active: PanelId::Left,
            theme: Theme::default(),
            command_line: String::new(),
            status: String::new(),
            panels_hidden: false,
            backend: Arc::new(LocalBackend::new()),
            screensaver: Screensaver::new(screensaver),
            focus: Focus::Panel,
            mode: Mode::Normal,
            layout: LayoutCache::default(),
            quick_search: String::new(),
            last_search: std::time::Instant::now(),
            drag: None,
            last_click: None,
            right_dragged: false,
            mouse: None,
            splash_until: splash
                .then(|| std::time::Instant::now() + std::time::Duration::from_millis(1800)),
            should_quit: false,
            tx,
        }
    }

    fn idx(id: PanelId) -> usize {
        match id {
            PanelId::Left => 0,
            PanelId::Right => 1,
        }
    }

    fn active_panel_mut(&mut self) -> &mut Panel {
        let i = Self::idx(self.active);
        &mut self.panels[i]
    }

    /// Whether the splash is still on screen. Expiry is checked here rather than
    /// on a timer tick, so a splash that has run out disappears on the next frame
    /// without needing one.
    pub(crate) fn splash_visible(&self) -> bool {
        self.splash_until
            .is_some_and(|t| std::time::Instant::now() < t)
    }

    /// Dismiss the splash. Returns `true` if it was showing, in which case the
    /// input that dismissed it must not also reach the application.
    fn dismiss_splash(&mut self) -> bool {
        if self.splash_visible() {
            self.splash_until = None;
            return true;
        }
        self.splash_until = None;
        false
    }

    /// The screensaver frame to draw, or `None` when the normal UI should be
    /// drawn. Called once per frame by the renderer.
    pub(crate) fn screensaver_canvas(&mut self, width: u16, height: u16) -> Option<&Canvas> {
        self.screensaver.update(width, height)
    }

    pub(crate) fn cwd_display(&self, id: PanelId) -> String {
        self.cwd[Self::idx(id)].display()
    }

    /// Kick off a listing for one panel. Returns immediately; entries arrive as
    /// [`Update::Entries`] messages, so a slow or hung backend never blocks the loop.
    fn reload(&mut self, id: PanelId) {
        let i = Self::idx(id);
        self.generation[i] += 1;
        let generation = self.generation[i];
        let path = self.cwd[i].clone();
        let backend = Arc::clone(&self.backend);
        let tx = self.tx.clone();

        self.panels[i].location = path.display();
        self.panels[i].set_entries(Vec::new());

        tokio::spawn(async move {
            // Bounded: if the UI falls behind, the walk waits instead of buffering
            // a million entries into memory.
            let (chunk_tx, mut chunk_rx) = mpsc::channel(4);
            let walk = {
                let path = path.clone();
                async move { backend.list(&path, chunk_tx).await }
            };
            let pump = async {
                while let Some(res) = chunk_rx.recv().await {
                    match res {
                        Ok(chunk) => {
                            if tx
                                .send(Update::Entries {
                                    panel: id,
                                    generation,
                                    chunk,
                                })
                                .is_err()
                            {
                                return; // UI is gone
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Update::Error {
                                panel: id,
                                message: e.to_string(),
                            });
                        }
                    }
                }
            };
            let (walk_result, ()) = tokio::join!(walk, pump);
            if let Err(e) = walk_result {
                let _ = tx.send(Update::Error {
                    panel: id,
                    message: e.to_string(),
                });
            }
        });
    }

    fn apply(&mut self, update: Update) {
        match update {
            Update::Entries {
                panel,
                generation,
                chunk,
            } => {
                let i = Self::idx(panel);
                // Stale result from a directory the user already left.
                if generation != self.generation[i] {
                    return;
                }
                let p = &mut self.panels[i];
                p.entries.extend(chunk.entries);
                if chunk.complete {
                    p.resort();
                }
            }
            Update::Error { panel, message } => {
                // Name the panel: with two of them, "permission denied" alone
                // leaves the user guessing which side failed.
                let side = match panel {
                    PanelId::Left => "left",
                    PanelId::Right => "right",
                };
                self.status = format!("{side} panel: {message}");
            }
        }
    }

    fn handle(&mut self, action: Action) {
        use Action::*;
        self.status.clear();

        match action {
            CursorUp => self.active_panel_mut().move_cursor(-1),
            CursorDown => self.active_panel_mut().move_cursor(1),
            PageUp => self.active_panel_mut().page(-1),
            PageDown => self.active_panel_mut().page(1),
            GoTop => self.active_panel_mut().go_home(),
            GoBottom => self.active_panel_mut().go_end(),

            Activate => self.activate(),
            GoParent => self.go_parent(),

            // Tab visits all three stops in order. This costs the strict
            // Tab-alternates-two-panels reflex from Norton Commander; Esc is the
            // fast two-way toggle that replaces it.
            FocusNext => {
                self.clear_quick_search();
                match (self.focus, self.active) {
                    (Focus::Panel, PanelId::Left) => self.active = PanelId::Right,
                    (Focus::Panel, PanelId::Right) => self.focus = Focus::CommandLine,
                    (Focus::CommandLine, _) => {
                        self.focus = Focus::Panel;
                        self.active = PanelId::Left;
                    }
                }
            }

            // Esc never changes which panel is current — that is the whole point
            // of having it alongside Tab.
            FocusToggle => {
                self.clear_quick_search();
                self.focus = match self.focus {
                    Focus::Panel => Focus::CommandLine,
                    Focus::CommandLine => Focus::Panel,
                };
            }

            SwitchPanel => self.active = self.active.other(),
            SwapPanels => {
                self.panels.swap(0, 1);
                self.cwd.swap(0, 1);
                self.generation.swap(0, 1);
            }
            TogglePanels => self.panels_hidden = !self.panels_hidden,
            Refresh => self.reload(self.active),

            ToggleSelection => self.active_panel_mut().toggle_selection(),
            InvertSelection => self.active_panel_mut().invert_selection(),
            ClearSelection => self.active_panel_mut().clear_selection(),
            SelectAll => {
                let p = self.active_panel_mut();
                p.clear_selection();
                p.invert_selection();
            }

            SortBy(key) => {
                let p = self.active_panel_mut();
                // Pressing the same sort key again reverses it, as in every
                // orthodox file manager.
                if p.sort_key == key {
                    p.sort_order = match p.sort_order {
                        SortOrder::Ascending => SortOrder::Descending,
                        SortOrder::Descending => SortOrder::Ascending,
                    };
                } else {
                    p.sort_key = key;
                    p.sort_order = SortOrder::Ascending;
                }
                p.resort();
            }
            ToggleHidden => {
                let p = self.active_panel_mut();
                p.show_hidden = !p.show_hidden;
                self.reload(self.active);
            }

            ScreensaverMenu => self.mode = Mode::Picker { selected: 0 },

            ContextMenu => {
                // Anchored on the cursor row, so the keyboard route opens the
                // menu next to what it acts on rather than in a corner.
                let i = Self::idx(self.active);
                let area = self.layout.panels[i];
                let row = (self.panels[i]
                    .cursor()
                    .saturating_sub(self.panels[i].offset())) as u16;
                self.open_context_menu((area.x + 2, area.y.saturating_add(row).saturating_add(1)));
            }

            Quit => self.should_quit = true,

            CommandChar(c) => self.command_line.push(c),
            CommandBackspace => {
                self.command_line.pop();
            }
            CommandClear => self.command_line.clear(),
            CommandSubmit => {
                if !self.command_line.is_empty() {
                    self.status = format!("shell not wired up yet: {}", self.command_line);
                    self.command_line.clear();
                }
            }

            QuickSearch(c) => self.quick_search(c),
            QuickSearchBackspace => {
                self.quick_search.pop();
                if self.quick_search.is_empty() {
                    self.status.clear();
                } else {
                    let needle = self.quick_search.clone();
                    self.seek(&needle);
                }
            }

            // Everything below is claimed by the keymap but owned by an agent
            // that has not built it yet. Say so out loud rather than doing nothing.
            Help => self.status = "F1 help — not implemented yet".into(),
            UserMenu => self.status = "F2 user menu — not implemented yet".into(),
            View => self.status = "F3 viewer — dmac-view, not implemented yet".into(),
            Edit => self.status = "F4 editor — dmac-view, not implemented yet".into(),
            Copy => self.status = self.pending_op("F5 copy"),
            Move => self.status = self.pending_op("F6 move"),
            MakeDir => self.status = "F7 mkdir — not implemented yet".into(),
            Delete => self.status = self.pending_op("F8 delete"),
            Menu => self.status = "F9 menu — not implemented yet".into(),
            Unimplemented(what) => self.status = format!("{what} — not implemented yet"),
        }
    }

    /// The contextual commands for the current entry.
    ///
    /// Built fresh each time rather than filtered from a fixed list: a menu that
    /// offers Delete on `..` is a menu that will eventually delete the wrong thing.
    pub(crate) fn context_items(&self) -> Vec<crate::ui::menu::Item> {
        use crate::ui::menu::Item;
        let Some(entry) = self.panels[Self::idx(self.active)].current() else {
            return vec![Item::new("Refresh", "Ctrl-R")];
        };

        match entry.kind {
            dmac_core::EntryKind::Parent => vec![
                Item::new("Go up", "Enter"),
                Item::SEPARATOR,
                Item::new("Refresh", "Ctrl-R"),
                Item::new("Make directory", "F7"),
            ],
            dmac_core::EntryKind::Dir => vec![
                Item::new("Open", "Enter"),
                Item::SEPARATOR,
                Item::new("Copy", "F5"),
                Item::new("Move / Rename", "F6"),
                Item::new("Delete", "F8"),
                Item::SEPARATOR,
                Item::new("Select", "Ins"),
                Item::new("Copy path", ""),
            ],
            _ => vec![
                Item::new("Open", "Enter"),
                Item::new("View", "F3"),
                Item::new("Edit", "F4"),
                Item::SEPARATOR,
                Item::new("Copy", "F5"),
                Item::new("Move / Rename", "F6"),
                Item::new("Delete", "F8"),
                Item::SEPARATOR,
                Item::new("Select", "Ins"),
                Item::new("Copy path", ""),
            ],
        }
    }

    fn open_context_menu(&mut self, anchor: (u16, u16)) {
        let items = self.context_items();
        self.mode = Mode::Context {
            selected: crate::ui::menu::first_selectable(&items),
            anchor,
        };
    }

    /// Run the chosen row. Matching on the label keeps the menu declarative;
    /// an unknown label is reported rather than silently ignored.
    fn run_context_item(&mut self, selected: usize) {
        let items = self.context_items();
        let Some(item) = items.get(selected) else {
            self.mode = Mode::Normal;
            return;
        };
        let label = item.label;
        self.mode = Mode::Normal;

        match label {
            "Open" | "Go up" => self.activate(),
            "View" => self.handle(Action::View),
            "Edit" => self.handle(Action::Edit),
            "Copy" => self.handle(Action::Copy),
            "Move / Rename" => self.handle(Action::Move),
            "Delete" => self.handle(Action::Delete),
            "Make directory" => self.handle(Action::MakeDir),
            "Select" => self.handle(Action::ToggleSelection),
            "Refresh" => self.handle(Action::Refresh),
            "Copy path" => {
                let path = self.cwd_display(self.active);
                let name = self.panels[Self::idx(self.active)]
                    .current()
                    .map(|e| e.name.clone())
                    .unwrap_or_default();
                self.status = format!("{path}/{name} — clipboard not wired up yet");
            }
            other => self.status = format!("{other} — not implemented yet"),
        }
    }

    /// Incremental search within the focused panel.
    ///
    /// The buffer expires after a pause: a letter typed a minute later starts a
    /// new search rather than silently extending the old one, which is the
    /// behaviour that makes quick-search feel broken elsewhere.
    fn quick_search(&mut self, c: char) {
        const EXPIRY: std::time::Duration = std::time::Duration::from_millis(1200);
        if self.last_search.elapsed() > EXPIRY {
            self.quick_search.clear();
        }
        self.last_search = std::time::Instant::now();
        self.quick_search.push(c);

        let needle = self.quick_search.clone();
        self.seek(&needle);
    }

    fn seek(&mut self, needle: &str) {
        // Search from the top so extending the buffer narrows the same result
        // rather than skipping to the next match of the longer string.
        let i = Self::idx(self.active);
        let found = self.panels[i]
            .entries
            .iter()
            .position(|e| e.name.to_lowercase().starts_with(&needle.to_lowercase()));

        match found {
            Some(index) => {
                self.panels[i].move_to(index);
                self.status = format!("search: {needle}");
            }
            None => self.status = format!("search: {needle}  (no match)"),
        }
    }

    fn clear_quick_search(&mut self) {
        if !self.quick_search.is_empty() {
            self.quick_search.clear();
            self.status.clear();
        }
    }

    /// Report what an unimplemented operation *would* act on. Even before the
    /// engine exists, this proves the selection model is right.
    fn pending_op(&self, label: &str) -> String {
        let p = &self.panels[Self::idx(self.active)];
        let n = p.operands().len();
        format!("{label}: {n} item(s) selected — engine not implemented yet")
    }

    /// Route one terminal event.
    ///
    /// Order matters: a running effect gets first refusal (so a game keeps its
    /// arrow keys), then an open overlay, then the keymap.
    fn on_input(&mut self, ev: Event) {
        match ev {
            Event::Key(k) if k.kind == KeyEventKind::Press => self.on_key(k),
            Event::Mouse(m) => self.on_mouse(m),
            // A resize is picked up by the next draw; a focus change counts as
            // activity so the screensaver does not start under a window the user
            // is actively looking at.
            Event::Resize(..) | Event::FocusGained | Event::FocusLost => {
                self.screensaver.on_activity();
            }
            _ => {}
        }
    }

    fn on_key(&mut self, k: KeyEvent) {
        if self.dismiss_splash() {
            return;
        }
        match self.screensaver.on_key(effect_key(k)) {
            // A game used the key, or a screensaver was dismissed by it. Either
            // way the application must not also act on it — waking a screen is
            // not the gesture that confirms a delete.
            Wake::Consumed | Wake::Dismissed => return,
            Wake::Passthrough => {}
        }

        match self.mode {
            Mode::Picker { selected } => return self.picker_key(k, selected),
            Mode::Context { selected, anchor } => return self.context_key(k, selected, anchor),
            Mode::Normal => {}
        }

        if let Some(action) = keymap::resolve(k, self.focus) {
            self.handle(action);
        }
    }

    fn picker_key(&mut self, k: KeyEvent, selected: usize) {
        let catalog = dmac_fx::catalog();
        let rows = crate::ui::picker::row_count(catalog);

        match k.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            // Wrapping: a six-item list is faster to reach by wrapping than by
            // scrolling back up.
            KeyCode::Up => {
                self.mode = Mode::Picker {
                    selected: (selected + rows - 1) % rows,
                }
            }
            KeyCode::Down => {
                self.mode = Mode::Picker {
                    selected: (selected + 1) % rows,
                }
            }
            KeyCode::Home => self.mode = Mode::Picker { selected: 0 },
            KeyCode::End => self.mode = Mode::Picker { selected: rows - 1 },
            KeyCode::Enter => self.start_picked(selected),
            _ => {}
        }
    }

    fn context_key(&mut self, k: KeyEvent, selected: usize, anchor: (u16, u16)) {
        let items = self.context_items();
        match k.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Up => {
                if let Some(i) = crate::ui::menu::next_selectable(&items, selected, -1) {
                    self.mode = Mode::Context {
                        selected: i,
                        anchor,
                    };
                }
            }
            KeyCode::Down => {
                if let Some(i) = crate::ui::menu::next_selectable(&items, selected, 1) {
                    self.mode = Mode::Context {
                        selected: i,
                        anchor,
                    };
                }
            }
            KeyCode::Enter => self.run_context_item(selected),
            _ => {}
        }
    }

    fn start_picked(&mut self, selected: usize) {
        let catalog = dmac_fx::catalog();
        let name = crate::ui::picker::name_at(catalog, selected).to_string();
        self.mode = Mode::Normal;

        match name.as_str() {
            // These are modes, not effects: set them and start one immediately.
            "random" | "rotation" => {
                self.screensaver.set_effect(&name);
                self.screensaver.start_now(0, 0);
            }
            _ => match dmac_fx::build(&name) {
                Some(effect) => self.screensaver.start_with(effect, 0, 0),
                None => {
                    self.status = format!("unknown effect: {name}");
                    return;
                }
            },
        }
        self.status = match self.screensaver.current() {
            Some(running) => format!("screensaver: {running}"),
            None => "screensaver failed to start".into(),
        };
    }

    // ---- mouse ----

    /// Which panel a screen position falls in, and which row within it.
    ///
    /// The row is `None` for empty space below the last entry — but the panel is
    /// still reported, because clicking an empty panel must focus it. Returning
    /// `None` for the whole hit would leave the click acting on the *other*
    /// panel, which in a two-panel file manager is the worst available outcome.
    fn hit_test(&self, col: u16, row: u16) -> Option<(PanelId, Option<usize>)> {
        for (i, id) in [PanelId::Left, PanelId::Right].into_iter().enumerate() {
            let area = self.layout.panels[i];
            if area.width == 0 || area.height == 0 {
                continue;
            }
            if col >= area.x
                && col < area.x + area.width
                && row >= area.y
                && row < area.y + area.height
            {
                let index = self.panels[i].offset() + (row - area.y) as usize;
                return Some((id, (index < self.panels[i].len()).then_some(index)));
            }
        }
        None
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        // Never let mouse motion end a game; the idle clock still resets.
        self.screensaver.on_activity();
        self.mouse = Some((m.column, m.row));
        // A click dismisses the splash, but pointer motion alone does not —
        // brushing the mouse should not rob you of the version you were reading.
        if !matches!(m.kind, MouseEventKind::Moved) && self.dismiss_splash() {
            return;
        }
        if self.screensaver.is_active() || self.mode != Mode::Normal {
            self.mouse_overlay(m);
            return;
        }

        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                match self.hit_test(m.column, m.row) {
                    Some((id, row)) => {
                        // Focus first, whether or not a row was hit.
                        self.active = id;
                        self.focus = Focus::Panel;
                        self.clear_quick_search();
                        let Some(index) = row else { return };

                        self.active_panel_mut().move_to(index);
                        self.drag = Some(Drag {
                            panel: id,
                            anchor: index,
                            toggling: false,
                        });

                        // Double-click opens, as in every file manager since 1995.
                        let now = std::time::Instant::now();
                        let double = self.last_click.is_some_and(|(t, p, i)| {
                            p == id && i == index && now.duration_since(t).as_millis() < 400
                        });
                        if double {
                            self.last_click = None;
                            self.activate();
                        } else {
                            self.last_click = Some((now, id, index));
                        }
                    }
                    None => {
                        if m.row == self.layout.fkeys.y && self.layout.fkeys.height > 0 {
                            self.fkey_click(m.column);
                        } else if m.row == self.layout.command.y && self.layout.command.height > 0 {
                            // Clicking the command line is the mouse equivalent
                            // of Esc, and must put the cursor there.
                            self.focus = Focus::CommandLine;
                        }
                    }
                }
            }

            // Right button sweeps the selection, as in Total Commander.
            MouseEventKind::Down(MouseButton::Right) => {
                if let Some((id, Some(index))) = self.hit_test(m.column, m.row) {
                    self.active = id;
                    self.active_panel_mut().move_to(index);
                    self.active_panel_mut().toggle_selection();
                    // `toggle_selection` steps down; put the cursor back where
                    // the user actually clicked.
                    self.active_panel_mut().move_to(index);
                    self.drag = Some(Drag {
                        panel: id,
                        anchor: index,
                        toggling: true,
                    });
                }
            }

            MouseEventKind::Drag(button) => {
                if button == MouseButton::Right {
                    self.right_dragged = true;
                }
                if let Some(drag) = self.drag
                    && let Some((id, Some(index))) = self.hit_test(m.column, m.row)
                    && id == drag.panel
                {
                    self.sweep(drag, index);
                }
            }

            MouseEventKind::Up(_) => self.drag = None,

            // Any-motion tracking: the pointer is drawn by inverting the cell
            // under it, so we need to know where it is on every move.
            MouseEventKind::Moved => {}

            // Three rows per notch: one is sluggish, a full page is disorienting.
            MouseEventKind::ScrollUp => self.scroll_under_pointer(m, -3),
            MouseEventKind::ScrollDown => self.scroll_under_pointer(m, 3),

            _ => {}
        }
    }

    /// Select every row between the drag anchor and the current one.
    ///
    /// Recomputed from the anchor each time rather than accumulated, so a fast
    /// drag that skips rows still selects the whole range.
    fn sweep(&mut self, drag: Drag, current: usize) {
        let i = Self::idx(drag.panel);
        let (lo, hi) = if drag.anchor <= current {
            (drag.anchor, current)
        } else {
            (current, drag.anchor)
        };

        let panel = &mut self.panels[i];
        for index in lo..=hi.min(panel.len().saturating_sub(1)) {
            if let Some(e) = panel.entries.get_mut(index)
                && e.kind != dmac_core::EntryKind::Parent
            {
                e.selected = true;
            }
        }
        panel.move_to(current);
        let _ = drag.toggling;
    }

    fn scroll_under_pointer(&mut self, m: MouseEvent, delta: isize) {
        // Scroll the panel the pointer is over, not the active one — that is
        // what every other application does and fighting it is just annoying.
        for (i, id) in [PanelId::Left, PanelId::Right].into_iter().enumerate() {
            let a = self.layout.panels[i];
            if m.column >= a.x && m.column < a.x + a.width && m.row >= a.y && m.row < a.y + a.height
            {
                self.panel_at(id).move_cursor(delta);
                return;
            }
        }
        self.active_panel_mut().move_cursor(delta);
    }

    fn mouse_context(&mut self, m: MouseEvent) {
        let Mode::Context { selected, anchor } = self.mode else {
            return;
        };
        let area = self.layout.menu;
        let inside = area.width > 0
            && m.column >= area.x
            && m.column < area.x + area.width
            && m.row >= area.y
            && m.row < area.y + area.height;

        match m.kind {
            MouseEventKind::Moved if inside => {
                // Hover-to-highlight, as in every desktop context menu.
                let row = (m.row - area.y) as usize;
                let items = self.context_items();
                if items.get(row).is_some_and(|i| !i.separator) {
                    self.mode = Mode::Context {
                        selected: row,
                        anchor,
                    };
                }
            }
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Down(MouseButton::Right) => {
                if !inside {
                    self.mode = Mode::Normal;
                    return;
                }
                let row = (m.row - area.y) as usize;
                let items = self.context_items();
                if items.get(row).is_some_and(|i| !i.separator) {
                    self.run_context_item(row);
                } else {
                    let _ = selected;
                }
            }
            _ => {}
        }
    }

    fn panel_at(&mut self, id: PanelId) -> &mut Panel {
        &mut self.panels[Self::idx(id)]
    }

    /// The F-key bar is clickable: each label occupies an equal share of the width.
    fn fkey_click(&mut self, column: u16) {
        let width = self.layout.fkeys.width.max(1);
        let cell = (width / 10).max(1);
        let n = ((column.saturating_sub(self.layout.fkeys.x) / cell) + 1).min(10);
        let action = match n {
            1 => Action::Help,
            2 => Action::UserMenu,
            3 => Action::View,
            4 => Action::Edit,
            5 => Action::Copy,
            6 => Action::Move,
            7 => Action::MakeDir,
            8 => Action::Delete,
            9 => Action::Menu,
            _ => Action::Quit,
        };
        self.handle(action);
    }

    /// Mouse handling while an overlay or the screensaver owns the screen.
    fn mouse_overlay(&mut self, m: MouseEvent) {
        if let Mode::Context { .. } = self.mode {
            return self.mouse_context(m);
        }
        let Mode::Picker { selected } = self.mode else {
            return;
        };
        let catalog = dmac_fx::catalog();
        let rows = crate::ui::picker::row_count(catalog);
        let area = self.layout.picker;

        match m.kind {
            MouseEventKind::ScrollUp => {
                self.mode = Mode::Picker {
                    selected: (selected + rows - 1) % rows,
                }
            }
            MouseEventKind::ScrollDown => {
                self.mode = Mode::Picker {
                    selected: (selected + 1) % rows,
                }
            }

            MouseEventKind::Down(MouseButton::Left) => {
                let inside = area.width > 0
                    && m.column >= area.x
                    && m.column < area.x + area.width
                    && m.row >= area.y
                    && m.row < area.y + area.height;

                if !inside {
                    // Clicking outside a menu closes it. Every other program does
                    // this, and a menu you can only escape with Esc feels stuck.
                    self.mode = Mode::Normal;
                    return;
                }

                // The games separator is a row but not a choice, so this maps
                // through the same helper the renderer uses.
                let clicked = (m.row - area.y) as usize;
                if let Some(index) = crate::ui::picker::index_at_row(catalog, clicked) {
                    if index == selected {
                        self.start_picked(index); // click the highlighted row to start
                    } else {
                        self.mode = Mode::Picker { selected: index };
                    }
                }
            }

            _ => {}
        }
    }

    fn activate(&mut self) {
        let i = Self::idx(self.active);
        let Some(entry) = self.panels[i].current() else {
            return;
        };

        match entry.kind {
            dmac_core::EntryKind::Parent => self.go_parent(),
            dmac_core::EntryKind::Dir => {
                // `join` rejects traversal, so a hostile listing entry named
                // `../..` cannot walk us out of the tree.
                if let Some(next) = self.cwd[i].join(&entry.name) {
                    self.cwd[i] = next;
                    self.reload(self.active);
                } else {
                    self.status = format!("refusing to enter suspicious name: {}", entry.name);
                }
            }
            _ => {
                self.status = format!("open {} — not wired up yet", entry.name);
            }
        }
    }

    fn go_parent(&mut self) {
        let i = Self::idx(self.active);
        if let Some(parent) = self.cwd[i].parent() {
            self.cwd[i] = parent;
            self.reload(self.active);
        }
    }
}

/// Translate a terminal key into the backend-agnostic form effects understand.
///
/// `dmac-fx` cannot see a crossterm `KeyEvent` — it sits below the TUI in the
/// layering, and its effects must also run under the GPU backend.
fn effect_key(k: KeyEvent) -> EffectKey {
    match k.code {
        KeyCode::Up => EffectKey::Up,
        KeyCode::Down => EffectKey::Down,
        KeyCode::Left => EffectKey::Left,
        KeyCode::Right => EffectKey::Right,
        KeyCode::Enter => EffectKey::Enter,
        KeyCode::Esc => EffectKey::Esc,
        KeyCode::Char(' ') => EffectKey::Space,
        KeyCode::Char(c) => EffectKey::Char(c),
        _ => EffectKey::Other,
    }
}

/// Run the application until the user quits.
pub async fn run(
    left: VfsPath,
    right: VfsPath,
    screensaver: ScreensaverConfig,
    splash: bool,
) -> anyhow::Result<()> {
    let mut guard = TerminalGuard::enter()?;

    let (update_tx, mut update_rx) = mpsc::unbounded_channel();
    let mut app = App::new(left, right, screensaver, splash, update_tx);
    app.reload(PanelId::Left);
    app.reload(PanelId::Right);

    // Input lives on its own thread doing a blocking read. Cheaper and more
    // portable than an async event stream, and it keeps the loop below free of
    // polling timeouts — we redraw when something happens, not on a timer.
    let (input_tx, mut input_rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        while let Ok(ev) = ratatui::crossterm::event::read() {
            if input_tx.send(ev).is_err() {
                break;
            }
        }
    });

    loop {
        guard.terminal().draw(|f| ui::draw(f, &mut app))?;

        // Exactly one timer, and only when something actually needs waking:
        // the idle deadline, or the next animation frame. `None` means we block
        // on input alone and cost nothing at all.
        // One timer for everything that needs waking: the splash expiry and the
        // screensaver, whichever comes first. Still `None` when neither wants one.
        let wake = match (app.splash_until, app.screensaver.deadline()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let sleep = async {
            match wake {
                Some(at) => tokio::time::sleep_until(at.into()).await,
                None => std::future::pending().await,
            }
        };

        tokio::select! {
            () = sleep => {}   // fall through to redraw the next frame
            Some(ev) = input_rx.recv() => {
                app.on_input(ev);
                // Drain the rest of the burst before redrawing, so holding an
                // arrow key does not queue one full frame per repeat.
                while let Ok(ev) = input_rx.try_recv() {
                    app.on_input(ev);
                }
            }
            Some(update) = update_rx.recv() => {
                app.apply(update);
                while let Ok(u) = update_rx.try_recv() {
                    app.apply(u);
                }
            }
            else => break,
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Build an App without a terminal, with a listing already in place.
    fn fixture() -> App {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(
            VfsPath::local("/left"),
            VfsPath::local("/right"),
            // Tests must never have a screensaver appear mid-assertion.
            ScreensaverConfig {
                enabled: false,
                ..Default::default()
            },
            false, // no splash: it would swallow the first key of every test
            tx,
        );
        let mk = |name: &str, kind| dmac_core::Entry {
            name: name.into(),
            kind,
            size: Some(1234),
            modified: None,
            mode: None,
            selected: false,
        };
        use dmac_core::EntryKind::*;
        app.panels[0].set_entries(vec![
            dmac_core::Entry::parent(),
            mk("src", Dir),
            mk("Cargo.toml", File),
            // The two cases that historically tear a panel border.
            mk("日本語のファイル.txt", File),
            mk("👨‍👩‍👧‍👦-family.png", File),
        ]);
        app
    }

    /// Render into a fixed-size buffer and assert nothing panicked and the
    /// frame is exactly the size we asked for. This is the guard that catches
    /// width-calculation regressions before a user sees a broken border.
    #[test]
    fn renders_without_panicking() {
        let mut app = fixture();
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();

        let buf = term.backend().buffer();
        assert_eq!(buf.area.width, 80);
        assert_eq!(buf.area.height, 24);
    }

    /// Every row of every line must be exactly the terminal width. A row that
    /// overflows is what produces the classic "border ate my filename" bug.
    #[test]
    fn no_row_overflows_the_terminal_width() {
        let mut app = fixture();
        for width in [20u16, 40, 80, 200] {
            let mut term = Terminal::new(TestBackend::new(width, 24)).unwrap();
            term.draw(|f| crate::ui::draw(f, &mut app))
                .unwrap_or_else(|e| panic!("draw failed at width {width}: {e}"));
            assert_eq!(term.backend().buffer().area.width, width);
        }
    }

    /// A terminal one cell tall is a real thing during a drag-resize. It must
    /// not panic.
    #[test]
    fn absurd_terminal_sizes_do_not_panic() {
        let mut app = fixture();
        for (w, h) in [(1u16, 1u16), (3, 2), (200, 1), (10, 3)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| crate::ui::draw(f, &mut app))
                .unwrap_or_else(|e| panic!("draw failed at {w}x{h}: {e}"));
        }
    }

    /// The F-key bar is the documentation; if it stops rendering, the app
    /// becomes unusable for anyone who has not memorised it.
    #[test]
    fn fkey_bar_is_always_present() {
        let mut app = fixture();
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();

        let buf = term.backend().buffer();
        let last: String = (0..80).map(|x| buf[(x, 23)].symbol().to_string()).collect();
        assert!(last.contains("Copy"), "F5 must be labelled: {last:?}");
        assert!(last.contains("Quit"), "F10 must be labelled: {last:?}");
    }

    #[test]
    fn f10_quits() {
        let mut app = fixture();
        assert!(!app.should_quit);
        app.handle(Action::Quit);
        assert!(app.should_quit);
    }

    #[test]
    fn tab_switches_the_active_panel() {
        let mut app = fixture();
        assert_eq!(app.active, PanelId::Left);
        app.handle(Action::SwitchPanel);
        assert_eq!(app.active, PanelId::Right);
    }

    // ---- mouse ----

    /// Give the app a plausible layout without going through a real draw.
    fn with_layout(mut app: App) -> App {
        app.layout.panels = [
            Rect {
                x: 1,
                y: 1,
                width: 38,
                height: 10,
            },
            Rect {
                x: 41,
                y: 1,
                width: 38,
                height: 10,
            },
        ];
        app.layout.fkeys = Rect {
            x: 0,
            y: 23,
            width: 80,
            height: 1,
        };
        // A real draw reports the viewport; without it the panel thinks it is
        // one row tall and every scroll offset is wrong.
        app.panels[0].set_viewport(10);
        app.panels[1].set_viewport(10);
        app
    }

    fn click(button: MouseButton, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(button),
            column,
            row,
            modifiers: ratatui::crossterm::event::KeyModifiers::NONE,
        }
    }

    fn drag_to(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column,
            row,
            modifiers: ratatui::crossterm::event::KeyModifiers::NONE,
        }
    }

    #[test]
    fn a_click_moves_the_cursor_to_the_clicked_row() {
        let mut app = with_layout(fixture());
        app.on_mouse(click(MouseButton::Left, 5, 3)); // interior row 2
        assert_eq!(app.active, PanelId::Left);
        assert_eq!(app.panels[0].cursor(), 2);
    }

    /// Clicking the inactive panel must focus it — otherwise the click acts on
    /// the wrong side, which is the worst possible outcome in a two-panel app.
    /// Clicking the other panel must focus it *even when it is empty* — otherwise
    /// the next keystroke acts on the panel the user just clicked away from.
    #[test]
    fn clicking_the_other_panel_activates_it_even_when_empty() {
        let mut app = with_layout(fixture());
        assert_eq!(app.active, PanelId::Left);
        assert!(app.panels[1].is_empty());
        app.on_mouse(click(MouseButton::Left, 45, 2));
        assert_eq!(app.active, PanelId::Right);
    }

    #[test]
    fn clicking_past_the_last_entry_does_nothing() {
        let mut app = with_layout(fixture());
        app.panels[0].move_to(1);
        app.on_mouse(click(MouseButton::Left, 5, 9)); // below the 5 entries
        assert_eq!(app.panels[0].cursor(), 1, "cursor must not move");
    }

    #[test]
    fn a_right_click_toggles_selection_and_leaves_the_cursor_put() {
        let mut app = with_layout(fixture());
        app.on_mouse(click(MouseButton::Right, 5, 3));
        assert!(app.panels[0].entries[2].selected);
        assert_eq!(
            app.panels[0].cursor(),
            2,
            "the cursor must stay where clicked"
        );
    }

    /// Dragging selects the whole swept range, including rows the mouse jumped
    /// over when it moved faster than events arrived.
    #[test]
    fn dragging_selects_the_whole_swept_range() {
        let mut app = with_layout(fixture());
        app.on_mouse(click(MouseButton::Left, 5, 2)); // row 1
        app.on_mouse(drag_to(5, 5)); // straight to row 4, skipping 2 and 3
        for i in 1..=4 {
            assert!(app.panels[0].entries[i].selected, "row {i} was skipped");
        }
    }

    #[test]
    fn dragging_upwards_selects_the_same_range() {
        let mut app = with_layout(fixture());
        app.on_mouse(click(MouseButton::Left, 5, 5)); // row 4
        app.on_mouse(drag_to(5, 2)); // back up to row 1
        for i in 1..=4 {
            assert!(app.panels[0].entries[i].selected, "row {i} was skipped");
        }
    }

    #[test]
    fn a_drag_never_selects_the_parent_row() {
        let mut app = with_layout(fixture());
        app.on_mouse(click(MouseButton::Left, 5, 5));
        app.on_mouse(drag_to(5, 1)); // sweeps across `..`
        assert!(
            !app.panels[0].entries[0].selected,
            "`..` must never be selectable"
        );
    }

    #[test]
    fn a_drag_does_not_leak_into_the_other_panel() {
        let mut app = with_layout(fixture());
        app.on_mouse(click(MouseButton::Left, 5, 2));
        app.on_mouse(drag_to(45, 5)); // pointer crosses into the right panel
        assert!(
            app.panels[1].entries.iter().all(|e| !e.selected),
            "a drag started in one panel must not select in the other"
        );
    }

    #[test]
    fn releasing_the_button_ends_the_drag() {
        let mut app = with_layout(fixture());
        app.on_mouse(click(MouseButton::Left, 5, 2));
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 5,
            row: 2,
            modifiers: ratatui::crossterm::event::KeyModifiers::NONE,
        });
        app.on_mouse(drag_to(5, 5));
        assert!(
            !app.panels[0].entries[4].selected,
            "the drag should be over"
        );
    }

    #[test]
    fn the_wheel_scrolls_the_panel_under_the_pointer_not_the_active_one() {
        let mut app = with_layout(fixture());
        // Give the right panel enough rows to scroll.
        let many: Vec<dmac_core::Entry> = (0..50)
            .map(|i| dmac_core::Entry {
                name: format!("f{i:03}"),
                kind: dmac_core::EntryKind::File,
                size: None,
                modified: None,
                mode: None,
                selected: false,
            })
            .collect();
        app.panels[1].set_entries(many);
        app.panels[1].set_viewport(10);

        app.on_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 45,
            row: 5,
            modifiers: ratatui::crossterm::event::KeyModifiers::NONE,
        });
        assert_eq!(app.active, PanelId::Left, "scrolling must not steal focus");
        assert!(app.panels[1].cursor() > 0, "the hovered panel must scroll");
        assert_eq!(app.panels[0].cursor(), 0);
    }

    #[test]
    fn clicking_the_last_cell_of_the_fkey_bar_quits() {
        let mut app = with_layout(fixture());
        app.on_mouse(click(MouseButton::Left, 79, 23));
        assert!(app.should_quit, "the rightmost cell is F10");
    }

    #[test]
    fn clicking_the_fifth_cell_of_the_fkey_bar_is_copy() {
        let mut app = with_layout(fixture());
        app.on_mouse(click(MouseButton::Left, 36, 23)); // 36/8 = cell 4 -> F5
        assert!(app.status.contains("F5 copy"), "got {:?}", app.status);
    }

    // ---- the picker ----

    #[test]
    fn the_menu_key_opens_the_picker_and_esc_closes_it() {
        let mut app = fixture();
        app.handle(Action::ScreensaverMenu);
        assert_eq!(app.mode, Mode::Picker { selected: 0 });
        app.picker_key(
            KeyEvent::new(KeyCode::Esc, ratatui::crossterm::event::KeyModifiers::NONE),
            0,
        );
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn picker_navigation_wraps_at_both_ends() {
        let mut app = fixture();
        let rows = crate::ui::picker::row_count(dmac_fx::catalog());
        app.handle(Action::ScreensaverMenu);
        app.picker_key(
            KeyEvent::new(KeyCode::Up, ratatui::crossterm::event::KeyModifiers::NONE),
            0,
        );
        assert_eq!(app.mode, Mode::Picker { selected: rows - 1 });
        app.picker_key(
            KeyEvent::new(KeyCode::Down, ratatui::crossterm::event::KeyModifiers::NONE),
            rows - 1,
        );
        assert_eq!(app.mode, Mode::Picker { selected: 0 });
    }

    #[test]
    fn choosing_a_game_from_the_picker_starts_it() {
        let mut app = fixture();
        let catalog = dmac_fx::catalog();
        let snake = (0..crate::ui::picker::row_count(catalog))
            .find(|&i| crate::ui::picker::name_at(catalog, i) == "snake")
            .expect("snake is in the catalog");
        app.start_picked(snake);
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.screensaver.current(), Some("snake"));
    }

    /// While a game runs, arrow keys steer it — they must not reach the panels.
    #[test]
    fn a_running_game_keeps_the_arrow_keys() {
        let mut app = fixture();
        app.screensaver
            .start_with(dmac_fx::build("snake").expect("snake"), 60, 20);
        let before = app.panels[0].cursor();
        app.on_key(KeyEvent::new(
            KeyCode::Down,
            ratatui::crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(app.panels[0].cursor(), before, "the game must keep the key");
        assert!(app.screensaver.is_active());
    }

    /// And the key that dismisses a screensaver must not also act on the panels.
    #[test]
    fn the_key_that_wakes_the_screen_does_not_reach_the_panels() {
        let mut app = fixture();
        app.screensaver
            .start_with(dmac_fx::build("matrix").expect("matrix"), 60, 20);
        let before = app.panels[0].cursor();
        app.on_key(KeyEvent::new(
            KeyCode::Down,
            ratatui::crossterm::event::KeyModifiers::NONE,
        ));
        assert!(!app.screensaver.is_active(), "it must be dismissed");
        assert_eq!(
            app.panels[0].cursor(),
            before,
            "and the key must be swallowed"
        );
    }

    /// Pressing the same sort key twice reverses the order rather than being a
    /// no-op — this is how every orthodox file manager behaves.
    #[test]
    fn repeating_a_sort_key_reverses_it() {
        let mut app = fixture();
        app.handle(Action::SortBy(dmac_core::SortKey::Size));
        assert_eq!(app.panels[0].sort_order, SortOrder::Ascending);
        app.handle(Action::SortBy(dmac_core::SortKey::Size));
        assert_eq!(app.panels[0].sort_order, SortOrder::Descending);
    }
}
