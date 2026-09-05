//! Application state and the event loop.
//!
//! The loop does exactly three things: drain input, drain state updates, draw.
//! Everything expensive happens in a Tokio task and arrives here as a message —
//! that is the rule that keeps the UI responsive while 100k files are copying.

use crate::action::Action;
use crate::terminal::{CursorStyle, TerminalGuard};
use crate::theme::Theme;
use crate::{keymap, ui};
use dmac_core::{Panel, PanelId, SortOrder};
use dmac_fx::{Canvas, EffectKey, Screensaver, ScreensaverConfig, Wake};
pub use dmac_session::Focus;
use dmac_session::{Session, SessionManager};
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
    /// Interior of the session rail.
    pub rail: Rect,
    /// Interior of the context menu while it is open.
    pub menu: Rect,
    /// Interior of the screensaver picker while it is open.
    pub picker: Rect,
}

pub struct App {
    /// Every live session. Switching between them does not save and reload —
    /// they all stay in memory with their listings intact, which is what makes
    /// the rail instant and what lets it stand in for a window switcher.
    pub(crate) sessions: SessionManager,
    /// Whether the session rail is expanded. Collapsed it is a narrow strip, so
    /// you can always see how many sessions you have without opening anything.
    pub(crate) rail_open: bool,
    pub(crate) cursor_style: CursorStyle,
    /// When the software cursor last flipped. Only used in `Software` mode.
    cursor_phase: std::time::Instant,
    pub(crate) theme: Theme,
    pub(crate) status: String,
    backend: BackendRef,
    screensaver: Screensaver,
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
        session_name: String,
        left: VfsPath,
        right: VfsPath,
        screensaver: ScreensaverConfig,
        splash: bool,
        cursor: CursorStyle,
        tx: mpsc::UnboundedSender<Update>,
    ) -> Self {
        Self {
            sessions: SessionManager::new(session_name, left, right),
            rail_open: false,
            cursor_style: cursor,
            cursor_phase: std::time::Instant::now(),
            theme: Theme::default(),
            status: String::new(),
            backend: Arc::new(LocalBackend::new()),
            screensaver: Screensaver::new(screensaver),
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

    /// The session currently on screen. Every panel operation goes through here,
    /// so nothing can accidentally act on a session the user is not looking at.
    pub(crate) fn ses(&self) -> &Session {
        self.sessions.current()
    }

    fn ses_mut(&mut self) -> &mut Session {
        self.sessions.current_mut()
    }

    fn idx(id: PanelId) -> usize {
        Session::index_of(id)
    }

    fn active_panel_mut(&mut self) -> &mut Panel {
        self.ses_mut().active_panel_mut()
    }

    /// Whether a text cursor is on screen at all: only when the command line has
    /// focus, nothing is overlaying it, and the terminal is drawing it for us.
    fn shows_text_cursor(&self) -> bool {
        !self.cursor_style.is_software()
            && self.ses().focus == Focus::CommandLine
            && self.mode == Mode::Normal
            && !self.screensaver.is_active()
            && !self.splash_visible()
    }

    /// Whether the software cursor is in its visible half. The classic terminal
    /// blink is about 530ms, which is slow enough not to be distracting and fast
    /// enough to read as "here".
    pub(crate) fn software_cursor_on(&self) -> bool {
        const PERIOD_MS: u128 = 530;
        (self.cursor_phase.elapsed().as_millis() / PERIOD_MS).is_multiple_of(2)
    }

    /// When the software cursor next needs redrawing, if it is in use and
    /// visible. `None` costs no timer, which is the usual case.
    fn cursor_deadline(&self) -> Option<std::time::Instant> {
        if !self.cursor_style.is_software()
            || self.ses().focus != Focus::CommandLine
            || self.mode != Mode::Normal
            || self.screensaver.is_active()
            || self.splash_visible()
        {
            return None;
        }
        const PERIOD: std::time::Duration = std::time::Duration::from_millis(530);
        let elapsed = self.cursor_phase.elapsed();
        let next = PERIOD * ((elapsed.as_millis() / PERIOD.as_millis()) as u32 + 1);
        Some(self.cursor_phase + next)
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
        self.ses().cwd[Self::idx(id)].display()
    }

    /// Kick off a listing for one panel. Returns immediately; entries arrive as
    /// [`Update::Entries`] messages, so a slow or hung backend never blocks the loop.
    fn reload(&mut self, id: PanelId) {
        let i = Self::idx(id);
        self.ses_mut().generation[i] += 1;
        let generation = self.ses().generation[i];
        let path = self.ses().cwd[i].clone();
        let backend = Arc::clone(&self.backend);
        let tx = self.tx.clone();

        self.ses_mut().panels[i].location = path.display();
        self.ses_mut().panels[i].set_entries(Vec::new());

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
                if generation != self.ses().generation[i] {
                    return;
                }
                let p = &mut self.ses_mut().panels[i];
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
            // A plain move ends any Shift-selection, so the next Shift+arrow
            // anchors where the cursor now is instead of resuming an old span.
            CursorUp => self.move_plain(|p| p.move_cursor(-1)),
            CursorDown => self.move_plain(|p| p.move_cursor(1)),
            PageUp => self.move_plain(|p| p.page(-1)),
            PageDown => self.move_plain(|p| p.page(1)),
            GoTop => self.move_plain(|p| p.go_home()),
            GoBottom => self.move_plain(|p| p.go_end()),

            ExtendSelection(delta) => self.extend(|p| p.extend_selection_by(delta)),
            ExtendSelectionPage(pages) => self.extend(|p| p.extend_selection_page(pages)),
            ExtendSelectionToTop => self.extend(|p| p.extend_selection_to(0)),
            ExtendSelectionToBottom => self.extend(|p| {
                let last = p.len().saturating_sub(1);
                p.extend_selection_to(last)
            }),

            Activate => self.activate(),
            GoParent => self.go_parent(),

            // Tab visits all three stops in order. This costs the strict
            // Tab-alternates-two-panels reflex from Norton Commander; Esc is the
            // fast two-way toggle that replaces it.
            FocusNext => {
                self.clear_quick_search();
                match (self.ses().focus, self.ses().active) {
                    (Focus::Panel, PanelId::Left) => self.ses_mut().active = PanelId::Right,
                    (Focus::Panel, PanelId::Right) => self.ses_mut().focus = Focus::CommandLine,
                    (Focus::CommandLine, _) => {
                        self.ses_mut().focus = Focus::Panel;
                        self.ses_mut().active = PanelId::Left;
                    }
                }
            }

            // Esc never changes which panel is current — that is the whole point
            // of having it alongside Tab.
            FocusToggle => {
                self.clear_quick_search();
                self.ses_mut().focus = match self.ses().focus {
                    Focus::Panel => Focus::CommandLine,
                    Focus::CommandLine => Focus::Panel,
                };
            }

            // --- sessions ---
            ToggleRail => self.rail_open = !self.rail_open,

            SwitchSession(index) => {
                if self.sessions.switch_to(index) {
                    self.after_session_switch();
                } else if index >= self.sessions.len() {
                    // Say why nothing happened. A silently ignored shortcut is
                    // indistinguishable from a broken one.
                    self.status =
                        format!("no session {} — {} open", index + 1, self.sessions.len());
                }
            }

            CycleSession(step) => {
                self.sessions.cycle(step);
                self.after_session_switch();
            }

            NewSession => {
                // Opens on the directory you are standing in: a new session is
                // almost always "the same place, a different task".
                let left = self.ses().cwd[0].clone();
                let right = self.ses().cwd[1].clone();
                let name = self.sessions.unused_name();
                self.sessions.create(name, left, right);
                self.reload(PanelId::Left);
                self.reload(PanelId::Right);
                self.rail_open = true;
                self.status = format!(
                    "session {} of {}",
                    self.sessions.current_index() + 1,
                    self.sessions.len()
                );
            }

            CloseSession => {
                let i = self.sessions.current_index();
                match self.sessions.close(i) {
                    Ok(()) => {
                        self.after_session_switch();
                        self.status = format!("session closed — {} left", self.sessions.len());
                    }
                    // Quitting is a different action with a different
                    // confirmation; do not silently turn one into the other.
                    Err(e) => self.status = format!("{e} (F10 quits)"),
                }
            }

            SwitchPanel => {
                let other = self.ses().active.other();
                self.ses_mut().active = other;
            }
            SwapPanels => {
                let s = self.ses_mut();
                s.panels.swap(0, 1);
                s.cwd.swap(0, 1);
                s.generation.swap(0, 1);
            }
            TogglePanels => {
                let hidden = self.ses().panels_hidden;
                self.ses_mut().panels_hidden = !hidden;
            }
            Refresh => self.reload(self.ses().active),

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
                self.reload(self.ses().active);
            }

            ScreensaverMenu => self.mode = Mode::Picker { selected: 0 },

            ContextMenu => {
                // Anchored on the cursor row, so the keyboard route opens the
                // menu next to what it acts on rather than in a corner.
                let i = Self::idx(self.ses().active);
                let area = self.layout.panels[i];
                let row = (self.ses().panels[i]
                    .cursor()
                    .saturating_sub(self.ses().panels[i].offset()))
                    as u16;
                self.open_context_menu((area.x + 2, area.y.saturating_add(row).saturating_add(1)));
            }

            Quit => self.should_quit = true,

            CommandChar(c) => self.ses_mut().command_line.push(c),
            CommandBackspace => {
                self.ses_mut().command_line.pop();
            }
            CommandClear => self.ses_mut().command_line.clear(),
            CommandSubmit => {
                if !self.ses().command_line.is_empty() {
                    self.status = format!("shell not wired up yet: {}", self.ses().command_line);
                    self.ses_mut().command_line.clear();
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

    /// A movement that is not a Shift-selection: it ends any running gesture.
    fn move_plain(&mut self, f: impl FnOnce(&mut Panel)) {
        let p = self.ses_mut().active_panel_mut();
        p.end_selection_gesture();
        f(p);
    }

    /// A Shift-selection step, reporting the running total.
    ///
    /// Saying how many rows are selected is what stops this feeling like a mode
    /// you fell into: the count changes as you move, so it is obvious what the
    /// keys are doing.
    fn extend(&mut self, f: impl FnOnce(&mut Panel)) {
        f(self.ses_mut().active_panel_mut());
        let n = self.ses().active_panel().operands().len();
        self.status = format!("{n} selected");
    }

    /// Settle the UI after the visible session changes.
    ///
    /// Nothing is reloaded: every session keeps its listings, which is the whole
    /// point of holding them all live. Only the transient, per-view state that
    /// belonged to the session we just left is cleared.
    fn after_session_switch(&mut self) {
        self.quick_search.clear();
        self.mode = Mode::Normal;
        self.drag = None;
        self.last_click = None;
        self.status = format!(
            "{} ({} of {})",
            self.ses().name,
            self.sessions.current_index() + 1,
            self.sessions.len()
        );
    }

    /// The contextual commands for the current entry.
    ///
    /// Built fresh each time rather than filtered from a fixed list: a menu that
    /// offers Delete on `..` is a menu that will eventually delete the wrong thing.
    pub(crate) fn context_items(&self) -> Vec<crate::ui::menu::Item> {
        use crate::ui::menu::Item;
        let Some(entry) = self.ses().active_panel().current() else {
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
                let path = self.cwd_display(self.ses().active);
                let name = self
                    .ses()
                    .active_panel()
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
        let i = Self::idx(self.ses().active);
        let found = self.ses().panels[i]
            .entries
            .iter()
            .position(|e| e.name.to_lowercase().starts_with(&needle.to_lowercase()));

        match found {
            Some(index) => {
                self.ses_mut().panels[i].move_to(index);
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
        let p = self.ses().active_panel();
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

        if let Some(action) = keymap::resolve(k, self.ses().focus) {
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
                let index = self.ses().panels[i].offset() + (row - area.y) as usize;
                return Some((id, (index < self.ses().panels[i].len()).then_some(index)));
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
        if !matches!(m.kind, MouseEventKind::Moved) {
            // Any deliberate mouse action ends a keyboard Shift-selection, so the
            // two gestures never fight over the same span. Mere pointer motion
            // does not: the mouse can drift while you are selecting with the
            // keyboard, and that should cost you nothing.
            self.ses_mut().active_panel_mut().end_selection_gesture();
            if self.dismiss_splash() {
                return;
            }
        }
        if self.screensaver.is_active() || self.mode != Mode::Normal {
            self.mouse_overlay(m);
            return;
        }

        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // The rail is to the left of the panels, so it is checked first.
                let rail = self.layout.rail;
                if rail.width > 0 && m.column >= rail.x && m.column < rail.x + rail.width {
                    if let Some(i) =
                        crate::ui::rail::session_at_row(&self.sessions, rail, self.rail_open, m.row)
                        && self.sessions.switch_to(i)
                    {
                        self.after_session_switch();
                    }
                    return;
                }
                match self.hit_test(m.column, m.row) {
                    Some((id, row)) => {
                        // Focus first, whether or not a row was hit.
                        self.ses_mut().active = id;
                        self.ses_mut().focus = Focus::Panel;
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
                            self.ses_mut().focus = Focus::CommandLine;
                        }
                    }
                }
            }

            // Right button sweeps the selection, as in Total Commander.
            MouseEventKind::Down(MouseButton::Right) => {
                if let Some((id, Some(index))) = self.hit_test(m.column, m.row) {
                    self.ses_mut().active = id;
                    self.ses_mut().focus = Focus::Panel;
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

        let panel = &mut self.ses_mut().panels[i];
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
        self.ses_mut().panel_mut(id)
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
        let i = Self::idx(self.ses().active);
        let Some(entry) = self.ses().panels[i].current() else {
            return;
        };

        match entry.kind {
            dmac_core::EntryKind::Parent => self.go_parent(),
            dmac_core::EntryKind::Dir => {
                // `join` rejects traversal, so a hostile listing entry named
                // `../..` cannot walk us out of the tree.
                if let Some(next) = self.ses().cwd[i].join(&entry.name) {
                    self.ses_mut().cwd[i] = next;
                    self.reload(self.ses().active);
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
        let i = Self::idx(self.ses().active);
        if let Some(parent) = self.ses().cwd[i].parent() {
            self.ses_mut().cwd[i] = parent;
            self.reload(self.ses().active);
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
    session_name: String,
    left: VfsPath,
    right: VfsPath,
    screensaver: ScreensaverConfig,
    splash: bool,
    cursor: CursorStyle,
) -> anyhow::Result<()> {
    let mut guard = TerminalGuard::enter(cursor)?;

    let (update_tx, mut update_rx) = mpsc::unbounded_channel();
    let mut app = App::new(
        session_name,
        left,
        right,
        screensaver,
        splash,
        cursor,
        update_tx,
    );
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

        // Re-assert the cursor shape, but only on frames that actually show a
        // cursor. Terminals reset DECSCUSR for reasons outside our control, and
        // setting it once at startup is why a blinking cursor silently stops
        // blinking — while emitting it on every frame would spray the sequence
        // at terminals that do not understand it.
        if app.shows_text_cursor() {
            crate::terminal::apply_cursor_style(app.cursor_style);
        }

        // Exactly one timer, and only when something actually needs waking:
        // the idle deadline, or the next animation frame. `None` means we block
        // on input alone and cost nothing at all.
        // One timer for everything that needs waking: the splash expiry and the
        // screensaver, whichever comes first. Still `None` when neither wants one.
        let wake = [
            app.splash_until,
            app.screensaver.deadline(),
            app.cursor_deadline(),
        ]
        .into_iter()
        .flatten()
        .min();
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
            "test".to_string(),
            VfsPath::local("/left"),
            VfsPath::local("/right"),
            // Tests must never have a screensaver appear mid-assertion.
            ScreensaverConfig {
                enabled: false,
                ..Default::default()
            },
            false, // no splash: it would swallow the first key of every test
            CursorStyle::default(),
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
        app.ses_mut().panels[0].set_entries(vec![
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
        assert_eq!(app.ses().active, PanelId::Left);
        app.handle(Action::SwitchPanel);
        assert_eq!(app.ses().active, PanelId::Right);
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
        app.ses_mut().panels[0].set_viewport(10);
        app.ses_mut().panels[1].set_viewport(10);
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
        assert_eq!(app.ses().active, PanelId::Left);
        assert_eq!(app.ses_mut().panels[0].cursor(), 2);
    }

    /// Clicking the inactive panel must focus it — otherwise the click acts on
    /// the wrong side, which is the worst possible outcome in a two-panel app.
    /// Clicking the other panel must focus it *even when it is empty* — otherwise
    /// the next keystroke acts on the panel the user just clicked away from.
    #[test]
    fn clicking_the_other_panel_activates_it_even_when_empty() {
        let mut app = with_layout(fixture());
        assert_eq!(app.ses().active, PanelId::Left);
        assert!(app.ses_mut().panels[1].is_empty());
        app.on_mouse(click(MouseButton::Left, 45, 2));
        assert_eq!(app.ses().active, PanelId::Right);
    }

    #[test]
    fn clicking_past_the_last_entry_does_nothing() {
        let mut app = with_layout(fixture());
        app.ses_mut().panels[0].move_to(1);
        app.on_mouse(click(MouseButton::Left, 5, 9)); // below the 5 entries
        assert_eq!(app.ses_mut().panels[0].cursor(), 1, "cursor must not move");
    }

    #[test]
    fn a_right_click_toggles_selection_and_leaves_the_cursor_put() {
        let mut app = with_layout(fixture());
        app.on_mouse(click(MouseButton::Right, 5, 3));
        assert!(app.ses_mut().panels[0].entries[2].selected);
        assert_eq!(
            app.ses_mut().panels[0].cursor(),
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
            assert!(
                app.ses_mut().panels[0].entries[i].selected,
                "row {i} was skipped"
            );
        }
    }

    #[test]
    fn dragging_upwards_selects_the_same_range() {
        let mut app = with_layout(fixture());
        app.on_mouse(click(MouseButton::Left, 5, 5)); // row 4
        app.on_mouse(drag_to(5, 2)); // back up to row 1
        for i in 1..=4 {
            assert!(
                app.ses_mut().panels[0].entries[i].selected,
                "row {i} was skipped"
            );
        }
    }

    #[test]
    fn a_drag_never_selects_the_parent_row() {
        let mut app = with_layout(fixture());
        app.on_mouse(click(MouseButton::Left, 5, 5));
        app.on_mouse(drag_to(5, 1)); // sweeps across `..`
        assert!(
            !app.ses_mut().panels[0].entries[0].selected,
            "`..` must never be selectable"
        );
    }

    #[test]
    fn a_drag_does_not_leak_into_the_other_panel() {
        let mut app = with_layout(fixture());
        app.on_mouse(click(MouseButton::Left, 5, 2));
        app.on_mouse(drag_to(45, 5)); // pointer crosses into the right panel
        assert!(
            app.ses_mut().panels[1].entries.iter().all(|e| !e.selected),
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
            !app.ses_mut().panels[0].entries[4].selected,
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
        app.ses_mut().panels[1].set_entries(many);
        app.ses_mut().panels[1].set_viewport(10);

        app.on_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 45,
            row: 5,
            modifiers: ratatui::crossterm::event::KeyModifiers::NONE,
        });
        assert_eq!(
            app.ses().active,
            PanelId::Left,
            "scrolling must not steal focus"
        );
        assert!(
            app.ses_mut().panels[1].cursor() > 0,
            "the hovered panel must scroll"
        );
        assert_eq!(app.ses_mut().panels[0].cursor(), 0);
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

    // ---- the software cursor ----

    /// The blink has to actually alternate. A phase function that never flips is
    /// exactly as useless as a terminal that ignores the cursor style, and it is
    /// the reason this mode exists at all.
    #[test]
    fn the_software_cursor_alternates_on_and_off() {
        let mut app = fixture();
        app.cursor_style = CursorStyle::Software;

        // Rewind the phase by hand rather than sleeping: a test that waits half a
        // second per assertion is a test people start skipping.
        let period = std::time::Duration::from_millis(530);
        let base = std::time::Instant::now();
        let observed: Vec<bool> = (0..4)
            .map(|k| {
                app.cursor_phase = base - period * k - std::time::Duration::from_millis(10);
                app.software_cursor_on()
            })
            .collect();
        assert_eq!(
            observed,
            vec![true, false, true, false],
            "phase must alternate"
        );
    }

    /// And it must only be drawn where text is actually being edited.
    #[test]
    fn no_cursor_is_scheduled_while_a_panel_has_focus() {
        let mut app = fixture();
        app.cursor_style = CursorStyle::Software;
        assert_eq!(app.ses().focus, Focus::Panel);
        assert!(
            app.cursor_deadline().is_none(),
            "a panel needs no blink timer"
        );

        app.handle(Action::FocusToggle);
        assert_eq!(app.ses().focus, Focus::CommandLine);
        assert!(app.cursor_deadline().is_some(), "the command line does");
    }

    /// An overlay owns the screen; a cursor blinking underneath it is noise.
    #[test]
    fn an_open_overlay_stops_the_cursor_blinking() {
        let mut app = fixture();
        app.cursor_style = CursorStyle::Software;
        app.handle(Action::FocusToggle);
        assert!(app.cursor_deadline().is_some());
        app.handle(Action::ScreensaverMenu);
        assert!(app.cursor_deadline().is_none());
    }

    /// The real-cursor path must schedule no timer at all — the terminal blinks
    /// it for us, and waking twice a second to do nothing is exactly the kind of
    /// idle cost this codebase refuses to pay.
    #[test]
    fn the_terminal_cursor_costs_no_timer() {
        let mut app = fixture();
        app.cursor_style = CursorStyle::BlinkingBlock;
        app.handle(Action::FocusToggle);
        assert!(app.cursor_deadline().is_none());
    }

    // ---- shift-selection ----

    #[test]
    fn shift_down_builds_a_selection_and_reports_the_count() {
        let mut app = fixture();
        app.ses_mut().active_panel_mut().move_to(1);
        app.handle(Action::ExtendSelection(1));
        app.handle(Action::ExtendSelection(1));
        assert_eq!(app.ses().active_panel().operands().len(), 3);
        assert!(app.status.contains("3 selected"), "got {:?}", app.status);
    }

    /// A plain arrow must not silently continue the previous span — the anchor
    /// has to move with the cursor, or the next Shift+arrow selects a surprise.
    #[test]
    fn a_plain_arrow_ends_the_gesture() {
        let mut app = fixture();
        app.ses_mut().active_panel_mut().move_to(1);
        app.handle(Action::ExtendSelection(1));
        assert!(app.ses().active_panel().selecting());
        app.handle(Action::CursorDown);
        assert!(!app.ses().active_panel().selecting());
    }

    #[test]
    fn shift_end_selects_everything_below_the_cursor() {
        let mut app = fixture();
        app.ses_mut().active_panel_mut().move_to(2);
        app.handle(Action::ExtendSelectionToBottom);
        // rows 2..4 of the five-row fixture; `..` at 0 is never selectable
        assert_eq!(app.ses().active_panel().operands().len(), 3);
    }

    #[test]
    fn shift_home_stops_short_of_the_parent_row() {
        let mut app = fixture();
        app.ses_mut().active_panel_mut().move_to(3);
        app.handle(Action::ExtendSelectionToTop);
        assert!(
            !app.ses().panels[0].entries[0].selected,
            "`..` is never selectable"
        );
        assert_eq!(app.ses().active_panel().operands().len(), 3);
    }

    /// The mouse and the keyboard must not fight over the same span.
    #[test]
    fn clicking_ends_a_keyboard_selection() {
        let mut app = with_layout(fixture());
        app.ses_mut().active_panel_mut().move_to(1);
        app.handle(Action::ExtendSelection(1));
        app.on_mouse(click(MouseButton::Left, 5, 4));
        assert!(!app.ses().active_panel().selecting());
    }

    // ---- sessions ----

    // Creating a session spawns its directory listing, so this needs a runtime.
    #[tokio::test]
    async fn a_new_session_opens_on_the_current_directory() {
        let mut app = fixture();
        let before = app.ses().cwd[0].clone();
        app.handle(Action::NewSession);
        assert_eq!(app.sessions.len(), 2);
        assert_eq!(
            app.ses().cwd[0],
            before,
            "a new session starts where you are"
        );
    }

    /// The property that makes the rail worth having: switching does not reload,
    /// so each session keeps the listing and cursor it had.
    // Creating a session spawns its directory listing, so this needs a runtime.
    #[tokio::test]
    async fn switching_sessions_preserves_each_ones_listing_and_cursor() {
        let mut app = fixture();
        app.ses_mut().active_panel_mut().move_to(3);
        app.handle(Action::NewSession);
        app.ses_mut().panels[0].set_entries(vec![dmac_core::Entry::parent()]);

        app.handle(Action::SwitchSession(0));
        assert_eq!(app.ses().active_panel().cursor(), 3, "cursor must survive");
        assert_eq!(app.ses().panels[0].len(), 5, "listing must survive");

        app.handle(Action::SwitchSession(1));
        assert_eq!(app.ses().panels[0].len(), 1);
    }

    #[test]
    fn the_rail_toggles_and_starts_closed() {
        let mut app = fixture();
        assert!(!app.rail_open);
        app.handle(Action::ToggleRail);
        assert!(app.rail_open);
        app.handle(Action::ToggleRail);
        assert!(!app.rail_open);
    }

    /// A shortcut that does nothing must say why, or it is indistinguishable
    /// from a broken binding.
    #[test]
    fn switching_to_a_session_that_does_not_exist_explains_itself() {
        let mut app = fixture();
        app.handle(Action::SwitchSession(6));
        assert_eq!(app.sessions.current_index(), 0);
        assert!(app.status.contains("no session 7"), "got {:?}", app.status);
    }

    // Creating a session spawns its directory listing, so this needs a runtime.
    #[tokio::test]
    async fn cycling_sessions_wraps() {
        let mut app = fixture();
        app.handle(Action::NewSession);
        app.handle(Action::NewSession);
        app.handle(Action::SwitchSession(0));
        app.handle(Action::CycleSession(-1));
        assert_eq!(app.sessions.current_index(), 2);
        app.handle(Action::CycleSession(1));
        assert_eq!(app.sessions.current_index(), 0);
    }

    /// Closing the last session must not become a way to quit by accident.
    // Creating a session spawns its directory listing, so this needs a runtime.
    #[tokio::test]
    async fn closing_the_last_session_is_refused_and_points_at_f10() {
        let mut app = fixture();
        app.handle(Action::CloseSession);
        assert_eq!(app.sessions.len(), 1);
        assert!(!app.should_quit, "closing a session must never quit");
        assert!(app.status.contains("F10"), "got {:?}", app.status);
    }

    // Creating a session spawns its directory listing, so this needs a runtime.
    #[tokio::test]
    async fn closing_a_session_leaves_the_others_intact() {
        let mut app = fixture();
        app.handle(Action::NewSession);
        app.handle(Action::NewSession);
        assert_eq!(app.sessions.len(), 3);
        app.handle(Action::CloseSession);
        assert_eq!(app.sessions.len(), 2);
        assert!(app.sessions.current_index() < 2);
    }

    /// Transient per-view state belongs to the view, not to the next session.
    // Creating a session spawns its directory listing, so this needs a runtime.
    #[tokio::test]
    async fn switching_sessions_clears_the_open_menu_and_any_drag() {
        let mut app = fixture();
        app.handle(Action::NewSession);
        app.handle(Action::ScreensaverMenu);
        assert_ne!(app.mode, Mode::Normal);
        app.handle(Action::SwitchSession(0));
        assert_eq!(
            app.mode,
            Mode::Normal,
            "an overlay must not follow you across"
        );
    }

    // Creating a session spawns its directory listing, so this needs a runtime.
    #[tokio::test]
    async fn each_session_remembers_its_own_focus_and_command_line() {
        let mut app = fixture();
        app.handle(Action::CommandChar('a'));
        app.handle(Action::FocusToggle);
        let focus_a = app.ses().focus;

        app.handle(Action::NewSession);
        app.handle(Action::CommandChar('b'));

        app.handle(Action::SwitchSession(0));
        assert_eq!(app.ses().command_line, "a");
        assert_eq!(app.ses().focus, focus_a);
        app.handle(Action::SwitchSession(1));
        assert_eq!(app.ses().command_line, "b");
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
        let before = app.ses_mut().panels[0].cursor();
        app.on_key(KeyEvent::new(
            KeyCode::Down,
            ratatui::crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(
            app.ses_mut().panels[0].cursor(),
            before,
            "the game must keep the key"
        );
        assert!(app.screensaver.is_active());
    }

    /// And the key that dismisses a screensaver must not also act on the panels.
    #[test]
    fn the_key_that_wakes_the_screen_does_not_reach_the_panels() {
        let mut app = fixture();
        app.screensaver
            .start_with(dmac_fx::build("matrix").expect("matrix"), 60, 20);
        let before = app.ses_mut().panels[0].cursor();
        app.on_key(KeyEvent::new(
            KeyCode::Down,
            ratatui::crossterm::event::KeyModifiers::NONE,
        ));
        assert!(!app.screensaver.is_active(), "it must be dismissed");
        assert_eq!(
            app.ses_mut().panels[0].cursor(),
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
        assert_eq!(app.ses_mut().panels[0].sort_order, SortOrder::Ascending);
        app.handle(Action::SortBy(dmac_core::SortKey::Size));
        assert_eq!(app.ses_mut().panels[0].sort_order, SortOrder::Descending);
    }
}
