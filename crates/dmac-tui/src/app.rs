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
use dmac_session::store::SessionStore;
use dmac_session::{Session, SessionId, SessionManager, View};
use dmac_vfs::{BackendRef, ListChunk, VfsPath, local::LocalBackend};
use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use std::sync::Arc;
use tokio::sync::mpsc;

/// A state change produced off the UI thread.
enum Update {
    /// A slice of a directory listing arrived.
    Entries {
        /// Which session asked for it.
        ///
        /// Routing by panel alone is not enough: sessions each have their own
        /// generation counter, so a stale result from one session can carry a
        /// generation that happens to match another's and be painted into the
        /// wrong workspace. This was doing exactly that.
        session: SessionId,
        panel: PanelId,
        /// Discarded if it does not match the panel's current generation — the
        /// user may have navigated away while the walk was in flight.
        generation: u64,
        chunk: ListChunk,
    },
    Error {
        session: SessionId,
        panel: PanelId,
        message: String,
    },
    /// Completion candidates for the command line came back.
    Completion {
        session: SessionId,
        /// Dropped unless it still matches: the user keeps typing while the
        /// directory is being read, and a completion for a word they have
        /// moved on from would rewrite the line under them.
        generation: u64,
        start: usize,
        items: Vec<String>,
    },
    /// A hosted shell changed what is on its screen. Carries nothing: the
    /// message exists only to break the event loop out of its wait, and the
    /// frame that follows reads the emulator directly.
    ShellOutput,
}

/// Everything the application needs to start.
///
/// A struct rather than nine parameters: they are all decided in one place and
/// arrive together, and this is the shape `dmac-config` will eventually produce
/// wholesale.
pub struct Startup {
    pub session_name: String,
    pub left: VfsPath,
    pub right: VfsPath,
    pub screensaver: ScreensaverConfig,
    pub splash: bool,
    pub cursor: CursorStyle,
    /// Where sessions are written. `None` disables persistence entirely.
    pub store: Option<SessionStore>,
    /// Sessions already read back from the store, and whether the last run
    /// exited cleanly.
    pub restored: Option<(SessionManager, bool)>,
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
    /// The session rail has the keyboard: it is a manager, not just a display.
    Rail {
        selected: usize,
    },
    /// A single-line text prompt.
    Prompt {
        intent: PromptIntent,
    },
    /// The utilities menu, with the highlighted row.
    Utilities {
        selected: usize,
    },
}

/// What a prompt is collecting. The value itself lives on `App`, because a
/// `Mode` is `Copy` and a growing string is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptIntent {
    RenameSession(usize),
    NewSession,
}

impl PromptIntent {
    pub(crate) fn title(self) -> &'static str {
        match self {
            PromptIntent::RenameSession(_) => " Rename session ",
            PromptIntent::NewSession => " New session ",
        }
    }
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
    /// Interior of the shell view while it is showing.
    pub shell: Rect,
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
    /// Text being typed into the current prompt.
    pub(crate) prompt_value: String,
    /// Where sessions are written. `None` disables persistence entirely, which
    /// is what `--no-session` and the tests use.
    store: Option<SessionStore>,
    /// Set when the session set changed; the loop flushes it, debounced, so a
    /// burst of edits costs one write rather than one per keystroke.
    dirty_at: Option<std::time::Instant>,
    /// Full screen: the frame stripped off, leaving only contents on black.
    pub(crate) fullscreen: bool,
    /// Bumped on every completion request, so a result for a word the user has
    /// already typed past is dropped instead of rewriting the line.
    completion_gen: u64,
    /// Escape sequences to hand the real terminal after the next frame.
    ///
    /// Queued rather than written where they are produced, because writing to
    /// stdout in the middle of composing a frame interleaves with what ratatui
    /// is emitting and corrupts both.
    pending_terminal_write: String,
    /// The last click over the hosted shell, for double-click word selection.
    /// Separate from `last_click`, which is about rows in a panel: a cell and a
    /// row are not the same thing and sharing the field would make a click in
    /// one look like a second click in the other.
    last_shell_click: Option<(std::time::Instant, u16, u16)>,
    /// Text selected on the command line, as char offsets (anchor, head).
    ///
    /// Characters, not bytes: every consumer wants to slice the string at these
    /// positions, and a byte offset in the middle of a multi-byte character
    /// panics rather than misbehaving quietly.
    pub(crate) command_selection: Option<(usize, usize)>,
    /// Text selected in the hosted shell, if any.
    pub(crate) shell_selection: Option<crate::ui::shell::Selection>,
    /// Whether the left button is still down on that selection. A drag in
    /// progress is protected from the repaint that would otherwise drop it.
    selecting: bool,
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
    /// When the hosted shell was last painted, so a program flooding stdout is
    /// throttled to a sane frame rate instead of redrawing as fast as the
    /// reader thread can parse.
    last_shell_frame: std::time::Instant,
    tx: mpsc::UnboundedSender<Update>,
}

/// Executables on `PATH` whose name starts with `word`.
///
/// The scan is done once and kept: `PATH` holds a dozen directories with a few
/// thousand files between them, and re-reading all of it on every Tab would
/// turn a keystroke into disk I/O for no new information.
fn commands_starting_with(word: &str) -> Vec<String> {
    static ALL: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    let all = ALL.get_or_init(|| {
        let mut out: Vec<String> = SHELL_BUILTINS.iter().map(|s| s.to_string()).collect();
        if let Some(path) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path) {
                let Ok(entries) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for e in entries.flatten() {
                    if is_executable(&e) {
                        out.push(e.file_name().to_string_lossy().into_owned());
                    }
                }
            }
        }
        out.sort();
        out.dedup();
        out
    });
    all.iter()
        .filter(|c| c.starts_with(word))
        .cloned()
        .collect()
}

/// Things a shell runs that are not files on `PATH`, so `cd` completes.
const SHELL_BUILTINS: &[&str] = &[
    "alias", "bg", "cd", "declare", "echo", "eval", "exec", "exit", "export", "fg", "history",
    "jobs", "kill", "let", "local", "popd", "pushd", "pwd", "read", "return", "set", "shift",
    "source", "test", "times", "trap", "type", "ulimit", "umask", "unalias", "unset", "wait",
];

#[cfg(unix)]
fn is_executable(e: &std::fs::DirEntry) -> bool {
    use std::os::unix::fs::PermissionsExt;
    e.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(e: &std::fs::DirEntry) -> bool {
    e.metadata().map(|m| m.is_file()).unwrap_or(false)
}

/// Filesystem entries matching `word`, as full replacement words.
///
/// The part of the word before the last `/` is kept exactly as typed — a `~`
/// the user wrote stays a `~`, because expanding it under them would rewrite a
/// path they can still read into one they have to re-check.
fn paths_starting_with(word: &str, cwd: &std::path::Path) -> Vec<String> {
    use dmac_core::complete::{escape, unescape};

    let (typed_dir, frag) = match word.rfind('/') {
        Some(i) => (&word[..=i], &word[i + 1..]),
        None => ("", word),
    };
    let frag = unescape(frag);

    let expanded = unescape(typed_dir);
    let dir: std::path::PathBuf = if let Some(rest) = expanded.strip_prefix("~/") {
        match home_dir() {
            Some(h) => h.join(rest),
            None => return Vec::new(),
        }
    } else if expanded.starts_with('/') {
        std::path::PathBuf::from(&expanded)
    } else if expanded.is_empty() {
        cwd.to_path_buf()
    } else {
        cwd.join(&expanded)
    };

    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with(&frag) {
            continue;
        }
        // A dotfile only shows once you have asked for one, as in every shell:
        // otherwise the first Tab in a home directory buries the answer.
        if name.starts_with('.') && !frag.starts_with('.') {
            continue;
        }
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        // A directory ends in `/` so the next Tab carries straight on into it;
        // a file ends in a space, because there is nothing more to say about it.
        let tail = if is_dir { "/" } else { " " };
        out.push(format!("{typed_dir}{}{tail}", escape(&name)));
        if out.len() >= 5000 {
            break;
        }
    }
    out
}

fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
}

/// The shortest gap between two frames of a hosted shell.
///
/// It bounds a flood, never a first response: the frame that answers a wake-up
/// is drawn immediately, and this only decides when the *next* one may follow.
const SHELL_FRAME_FLOOR: std::time::Duration = std::time::Duration::from_millis(16);

/// How close together two clicks have to be to count as one gesture.
const DOUBLE_CLICK: std::time::Duration = std::time::Duration::from_millis(400);

impl App {
    /// Private: `Update` is an internal message type, so the only supported
    /// entry point is [`run`].
    fn new(start: Startup, tx: mpsc::UnboundedSender<Update>) -> Self {
        let Startup {
            session_name,
            left,
            right,
            screensaver,
            splash,
            cursor,
            store,
            // Handled by `run`, which needs the terminal up before it can list
            // the restored sessions.
            restored: _,
        } = start;
        Self {
            sessions: SessionManager::new(session_name, left, right),
            rail_open: false,
            prompt_value: String::new(),
            store,
            dirty_at: None,
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
            fullscreen: false,
            completion_gen: 0,
            pending_terminal_write: String::new(),
            last_shell_click: None,
            command_selection: None,
            shell_selection: None,
            selecting: false,
            last_shell_frame: std::time::Instant::now(),
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
        !self.cursor_style.is_software() && self.wants_text_cursor()
    }

    /// The software cursor's state for this frame, or `None` when the real
    /// terminal cursor is being used instead.
    pub(crate) fn software_cursor(&self) -> Option<bool> {
        (self.cursor_style.is_software() && self.wants_text_cursor())
            .then(|| self.software_cursor_on())
    }

    /// Whether the software cursor is in its visible half. The classic terminal
    /// blink is about 530ms, which is slow enough not to be distracting and fast
    /// enough to read as "here".
    pub(crate) fn software_cursor_on(&self) -> bool {
        const PERIOD_MS: u128 = 530;
        (self.cursor_phase.elapsed().as_millis() / PERIOD_MS).is_multiple_of(2)
    }

    /// Whether a text cursor belongs on screen this frame, in either style.
    ///
    /// One predicate for both, because they must never disagree: a frame that
    /// draws a cursor and does not tell it to blink is exactly how a blinking
    /// cursor silently stops blinking.
    fn wants_text_cursor(&self) -> bool {
        if self.mode != Mode::Normal || self.screensaver.is_active() || self.splash_visible() {
            return false;
        }
        match self.ses().view {
            // The hosted program decides. An editor that hid its cursor must
            // stay without one — putting it back would be dmac overriding a
            // decision the program made about its own display.
            View::Shell => self.ses().hosted().is_some_and(|s| {
                !s.finished() && s.with_screen(|sc| !sc.hide_cursor()).unwrap_or(false)
            }),
            View::Panels => self.ses().focus == Focus::CommandLine,
        }
    }

    /// When the software cursor next needs redrawing, if it is in use and
    /// visible. `None` costs no timer, which is the usual case.
    fn cursor_deadline(&self) -> Option<std::time::Instant> {
        if !self.cursor_style.is_software() || !self.wants_text_cursor() {
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
    /// Start a listing for one panel of the session on screen.
    fn reload(&mut self, id: PanelId) {
        let i = self.sessions.current_index();
        self.reload_session(i, id);
    }

    /// Start a listing for any session, visible or not. Restoring a saved set
    /// needs this: every session is live, so every panel needs its contents.
    fn reload_session(&mut self, index: usize, id: PanelId) {
        if index >= self.sessions.len() {
            return;
        }
        let i = Self::idx(id);
        let target = self.sessions.at_mut(index);
        target.generation[i] += 1;
        let generation = target.generation[i];
        let path = target.cwd[i].clone();
        let session = target.id;

        target.panels[i].location = path.display();
        target.panels[i].set_entries(Vec::new());

        let backend = Arc::clone(&self.backend);
        let tx = self.tx.clone();

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
                                    session,
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
                                session,
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
                    session,
                    panel: id,
                    message: e.to_string(),
                });
            }
        });
    }

    fn apply(&mut self, update: Update) {
        match update {
            Update::Entries {
                session,
                panel,
                generation,
                chunk,
            } => {
                // Deliver to the session that asked, wherever it is in the list —
                // not to whichever session happens to be on screen now.
                let Some(target) = self.sessions.index_of_id(session) else {
                    return; // its session was closed while the walk was running
                };
                let i = Self::idx(panel);
                // Stale result from a directory that session already left.
                if generation != self.sessions.all()[target].generation[i] {
                    return;
                }
                let p = &mut self.sessions.at_mut(target).panels[i];
                p.entries.extend(chunk.entries);
                if chunk.complete {
                    p.resort();
                }
            }
            Update::Completion {
                session,
                generation,
                start,
                items,
            } => self.apply_completion(session, generation, start, items),
            // Nothing to apply: arriving here already cost the redraw that the
            // hosted program was asking for.
            Update::ShellOutput => {}
            Update::Error {
                session,
                panel,
                message,
            } => {
                // Only worth reporting if the user can see that session.
                if self.ses().id != session {
                    return;
                }
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
                // On the command line Tab completes what is being typed, and
                // only moves the keyboard when there is nothing to complete —
                // so it means what it means in a shell, without losing the way
                // out of the line.
                if self.ses().focus == Focus::CommandLine
                    && self.mode == Mode::Normal
                    && self.request_completion()
                {
                    return;
                }
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
            // Opening the rail hands it the keyboard: a list you can see but
            // not drive is a worse version of one you cannot see.
            ToggleRail => {
                if self.rail_open {
                    self.close_rail();
                } else {
                    self.rail_open = true;
                    self.mode = Mode::Rail {
                        selected: self.sessions.current_index(),
                    };
                    self.status =
                        "sessions: enter switch \u{b7} n new \u{b7} r rename \u{b7} d close".into();
                }
            }

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
            ToggleShell => self.toggle_shell(),
            ToggleFullscreen => self.toggle_fullscreen(),
            UtilitiesMenu => {
                self.mode = Mode::Utilities {
                    selected: crate::ui::menu::first_selectable(&crate::utilities::items()),
                };
            }
            ExtendCommandSelection(delta) => self.extend_command_selection(delta),
            ExtendCommandSelectionToStart => self.set_command_selection_head(0),
            ExtendCommandSelectionToEnd => {
                let end = self.ses().command_line.chars().count();
                self.set_command_selection_head(end);
            }
            ClipboardCopy => self.copy_selection(),
            ClipboardPaste => self.paste_into_shell(),
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

            // Typing replaces a selection the way every editor does, rather
            // than appending past it and leaving a highlight over stale text.
            CommandChar(c) => {
                self.take_command_selection();
                self.ses_mut().command_line.push(c);
            }
            CommandBackspace if self.command_selection.is_some() => {
                self.take_command_selection();
            }
            CommandBackspace => {
                self.ses_mut().command_line.pop();
            }
            CommandClear => {
                self.command_selection = None;
                self.ses_mut().command_line.clear();
            }
            CommandSubmit => self.run_command(),

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

    /// Note that the session set changed. The actual write is debounced by the
    /// event loop: renaming a session one keystroke at a time should not mean
    /// one file write per keystroke.
    fn touch_sessions(&mut self) {
        if self.store.is_some() {
            self.dirty_at = Some(std::time::Instant::now());
        }
    }

    /// Write now if the debounce has elapsed. Returns when the next flush is due.
    fn flush_sessions(&mut self) -> Option<std::time::Instant> {
        const DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(600);
        let due = self.dirty_at? + DEBOUNCE;
        if std::time::Instant::now() < due {
            return Some(due);
        }
        self.dirty_at = None;
        if let Some(store) = &self.store
            && let Err(e) = store.save(&self.sessions, false)
        {
            // Say so once rather than every 600ms: a read-only config directory
            // would otherwise fill the status line forever.
            self.status = format!("could not save sessions: {e}");
        }
        None
    }

    /// Final write, marking a clean exit so the next start knows we did not crash.
    fn save_on_exit(&mut self) {
        // What is running has to be looked at before anything is torn down:
        // afterwards there is nothing left to ask.
        for i in 0..self.sessions.len() {
            self.sessions.at_mut(i).agent = self.sessions.at_mut(i).running_agent();
        }
        if let Some(store) = &self.store
            && let Err(e) = store.save(&self.sessions, true)
        {
            eprintln!("dmac: could not save sessions: {e}");
        }
        // Saved first, then torn down: if shutting the processes down goes
        // wrong, the session layout is already on disk. The grace period is
        // long enough for a program to release what it is holding and short
        // enough not to be felt as a hang.
        self.sessions
            .shutdown(std::time::Duration::from_millis(400));
    }

    fn close_rail(&mut self) {
        self.rail_open = false;
        if matches!(self.mode, Mode::Rail { .. }) {
            self.mode = Mode::Normal;
        }
        self.status.clear();
    }

    /// The rail as a manager: navigate, switch, create, rename, close.
    fn rail_key(&mut self, k: KeyEvent, selected: usize) {
        let n = self.sessions.len();
        match k.code {
            KeyCode::Esc => self.close_rail(),
            KeyCode::Up => {
                self.mode = Mode::Rail {
                    selected: (selected + n - 1) % n,
                }
            }
            KeyCode::Down => {
                self.mode = Mode::Rail {
                    selected: (selected + 1) % n,
                }
            }
            KeyCode::Enter => {
                if self.sessions.switch_to(selected) {
                    self.after_session_switch();
                }
                self.close_rail();
            }
            KeyCode::Char('n') => self.open_prompt(PromptIntent::NewSession, String::new()),
            KeyCode::Char('r') => {
                let current = self
                    .sessions
                    .get(selected)
                    .map(|s| s.name.clone())
                    .unwrap_or_default();
                self.open_prompt(PromptIntent::RenameSession(selected), current);
            }
            KeyCode::Char('d') | KeyCode::Delete => match self.sessions.close(selected) {
                Ok(()) => {
                    let keep = selected.min(self.sessions.len() - 1);
                    self.mode = Mode::Rail { selected: keep };
                    self.after_session_switch();
                    self.status = format!("session closed \u{2014} {} left", self.sessions.len());
                }
                // Quitting is a different action with a different confirmation.
                Err(e) => self.status = format!("{e} (F10 quits)"),
            },
            // Bare digits while the rail has focus: the numbers are on screen
            // right there, so demanding a modifier would be perverse.
            KeyCode::Char(c @ '1'..='9') => {
                let i = c.to_digit(10).unwrap_or(1) as usize - 1;
                if self.sessions.switch_to(i) {
                    self.after_session_switch();
                }
                self.close_rail();
            }
            _ => {}
        }
    }

    fn open_prompt(&mut self, intent: PromptIntent, initial: String) {
        self.prompt_value = initial;
        self.mode = Mode::Prompt { intent };
    }

    fn prompt_key(&mut self, k: KeyEvent, intent: PromptIntent) {
        match k.code {
            KeyCode::Esc => {
                self.prompt_value.clear();
                // Back to the rail, not out to the panels: the prompt was opened
                // from there, and Esc means one level back.
                self.mode = Mode::Rail {
                    selected: self.sessions.current_index(),
                };
            }
            KeyCode::Backspace => {
                self.prompt_value.pop();
            }
            KeyCode::Enter => self.submit_prompt(intent),
            KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                self.prompt_value.push(c)
            }
            _ => {}
        }
    }

    fn submit_prompt(&mut self, intent: PromptIntent) {
        let value = std::mem::take(&mut self.prompt_value);
        match intent {
            PromptIntent::RenameSession(index) => match self.sessions.rename(index, &value) {
                Ok(()) => {
                    self.touch_sessions();
                    self.mode = Mode::Rail { selected: index };
                    self.status = format!("renamed to {}", value.trim());
                }
                // Stay in the prompt with the text intact, so a rejected name can
                // be corrected rather than retyped.
                Err(e) => {
                    self.prompt_value = value;
                    self.status = e.to_string();
                }
            },
            PromptIntent::NewSession => {
                let name = if value.trim().is_empty() {
                    self.sessions.unused_name()
                } else {
                    value.trim().to_string()
                };
                let left = self.ses().cwd[0].clone();
                let right = self.ses().cwd[1].clone();
                let i = self.sessions.create(name, left, right);
                self.reload(PanelId::Left);
                self.reload(PanelId::Right);
                self.touch_sessions();
                self.mode = Mode::Rail { selected: i };
                self.after_session_switch();
            }
        }
    }

    /// The area the shell is drawn in, in cells. Recorded by the renderer, so
    /// the PTY is always exactly the size of what the user can see.
    fn shell_size(&self) -> (u16, u16) {
        let a = self.layout.shell;
        if a.width >= 2 && a.height >= 2 {
            (a.width, a.height)
        } else {
            // Before the first frame there is no recorded size. Something
            // plausible beats zero: the child is told the truth on the next draw.
            (80, 24)
        }
    }

    /// Complete the word being typed on the command line.
    ///
    /// The candidates come from the filesystem, so they are gathered on a task
    /// and delivered like a directory listing. Tab has to stay instant even
    /// when the directory being completed against is a slow network mount.
    fn request_completion(&mut self) -> bool {
        let line = self.ses().command_line.clone();
        let (start, word) = dmac_core::complete::word_at_end(&line);
        // Nothing to complete: let Tab go back to being the focus key.
        if word.is_empty() {
            return false;
        }

        self.completion_gen = self.completion_gen.wrapping_add(1);
        let generation = self.completion_gen;
        let session = self.ses().id;
        let cwd = self.ses().cwd[Self::idx(self.ses().active)].clone();
        let first = dmac_core::complete::is_first_word(&line, start);
        let word = word.to_string();
        let tx = self.tx.clone();

        tokio::task::spawn_blocking(move || {
            let items = if first && !word.contains('/') {
                commands_starting_with(&word)
            } else {
                paths_starting_with(&word, cwd.as_path())
            };
            let _ = tx.send(Update::Completion {
                session,
                generation,
                start,
                items,
            });
        });
        true
    }

    fn apply_completion(
        &mut self,
        session: SessionId,
        generation: u64,
        start: usize,
        items: Vec<String>,
    ) {
        use dmac_core::complete::{Completion, resolve, splice};
        if generation != self.completion_gen || self.ses().id != session {
            return;
        }
        let line = self.ses().command_line.clone();
        // The line moved on while the directory was being read.
        if start > line.len() {
            return;
        }
        let word = &line[start..];

        match resolve(word, items) {
            Completion::None => self.status = "no match".into(),
            Completion::Single(full) => {
                self.ses_mut().command_line = splice(&line, start, &full);
                self.status.clear();
            }
            Completion::Many { prefix, items } => {
                self.ses_mut().command_line = splice(&line, start, &prefix);
                // bash prints the candidates rather than guessing; the status
                // line is where this program says things.
                let shown: Vec<&str> = items.iter().take(12).map(String::as_str).collect();
                let more = items.len().saturating_sub(shown.len());
                self.status = if more > 0 {
                    format!("{}  … and {more} more", shown.join("  "))
                } else {
                    shown.join("  ")
                };
            }
        }
    }

    /// A callback a hosted shell can use to ask for a repaint.
    ///
    /// This is the whole reason a hosted program is live rather than a picture
    /// that updates when you type: without it `ping`, `tail -f` or a running
    /// build produce output that nothing is waiting for.
    fn waker(&self) -> Option<dmac_pty::Waker> {
        let tx = self.tx.clone();
        Some(Arc::new(move || {
            let _ = tx.send(Update::ShellOutput);
        }))
    }

    /// Bookkeeping immediately before a frame is drawn.
    ///
    /// The flag is cleared *before* the screen is read, never after. Clearing it
    /// afterwards loses whatever the child printed while the frame was being
    /// composed: that output is already marked as shown but was never on it, and
    /// since the reader only wakes on the clean-to-dirty edge, nothing would ask
    /// again. The last line of a finished build would sit there invisible.
    /// Clearing first can only cost one redundant frame, which is the cheap
    /// direction to be wrong in.
    ///
    /// It is also the throttle: while the flag stays set the reader thread stays
    /// quiet, because a repaint has been asked for and not yet given.
    pub(crate) fn before_frame(&mut self) {
        if self.ses().view != View::Shell || self.last_shell_frame.elapsed() < SHELL_FRAME_FLOOR {
            return;
        }
        // The screen is about to change under the selection, and the selection
        // is in screen coordinates: keeping it would highlight whatever landed
        // in those cells. A drag in progress is the user's, and is left alone.
        if !self.selecting
            && self.shell_selection.is_some()
            && self.ses().hosted().is_some_and(|s| s.dirty())
        {
            self.shell_selection = None;
        }
        self.last_shell_frame = std::time::Instant::now();
        if let Some(s) = self.ses().hosted() {
            s.mark_drawn();
        }
    }

    /// When the hosted shell is owed a frame it has not been given yet.
    ///
    /// Only ever set while output is outstanding, so an idle shell costs no
    /// timer — the pane sits there for free until the child says something.
    fn shell_deadline(&self) -> Option<std::time::Instant> {
        if self.ses().view != View::Shell {
            return None;
        }
        self.ses()
            .hosted()
            .is_some_and(|s| s.dirty())
            .then(|| self.last_shell_frame + SHELL_FRAME_FLOOR)
    }

    /// Put back whatever was running in these sessions when they were saved.
    ///
    /// Started after the first frame, never before it: a cold start has 80ms to
    /// show something, and spawning shells inside that budget would spend it on
    /// work the user cannot see yet.
    pub(crate) fn reattach_agents(&mut self) {
        for i in 0..self.sessions.len() {
            let Some(saved) = self.sessions.at_mut(i).reattach.take() else {
                continue;
            };
            // Replayed as a resume: the saved line names the conversation it
            // *created*, and running it verbatim asks for one that already
            // exists. Every other argument the user chose is kept.
            let command = dmac_session::agent::as_resume(&saved);
            let program = command
                .split_whitespace()
                .next()
                .and_then(|w| w.rsplit('/').next())
                .unwrap_or("agent")
                .to_string();
            // Anything still holding this conversation is an orphan from a run
            // that did not get to clean up, and it is holding exactly what we
            // are about to ask for. Left alone it produces "that session is
            // already in use" on a fresh start.
            let conversation = self.sessions.at_mut(i).conversation_id().to_string();
            let cleared = dmac_session::agent::clear_orphans(&conversation);

            let waker = self.waker();
            let (cols, rows) = self.shell_size();
            let session = self.sessions.at_mut(i);
            match session.shell(cols, rows, waker) {
                Ok(shell) => {
                    // Just the program name: the shim on PATH turns it into a
                    // resume of this session's conversation.
                    if shell.run(&command).is_ok() {
                        // Show the shell: reattaching something and leaving the
                        // user on the panels hides the very thing just started.
                        session.view = dmac_session::View::Shell;
                        self.status = if cleared > 0 {
                            format!("{program} reattached — cleared {cleared} left over")
                        } else {
                            format!("{program} reattached")
                        };
                    }
                }
                Err(e) => self.status = format!("could not reattach {program}: {e}"),
            }
        }
    }

    /// Quit as if F10 had been pressed. Used when the terminal goes away.
    pub(crate) fn request_quit(&mut self) {
        self.should_quit = true;
    }

    /// Keep the hosted shell exactly the size of the pane showing it.
    ///
    /// A terminal program redraws itself when its terminal changes size, and it
    /// only knows because the terminal tells it. This used to happen lazily, on
    /// the way through `send_to_shell`, so a hosted `vim` or `htop` kept the old
    /// geometry until the user pressed a key — the window had been resized and
    /// the program inside it had not been told.
    ///
    /// Called after the frame, because the pane's size is what the renderer just
    /// recorded. The child repaints in its own time and its output asks for the
    /// frame that shows it.
    pub(crate) fn sync_shell_size(&mut self) {
        if self.ses().view != View::Shell {
            return;
        }
        let (cols, rows) = self.shell_size();
        if let Some(sh) = self.ses_mut().shell.as_mut() {
            // `resize` is a no-op when the size already matches, so this costs
            // nothing on the overwhelming majority of frames.
            let _ = sh.resize(cols, rows);
        }
    }

    /// Remove the selected text from the command line, if any, and forget the
    /// selection. Returns whether anything was removed.
    fn take_command_selection(&mut self) -> bool {
        let Some((lo, hi)) = self.command_selection_span() else {
            return false;
        };
        let line = &self.ses().command_line;
        let kept: String = line
            .chars()
            .enumerate()
            .filter(|(i, _)| *i < lo || *i >= hi)
            .map(|(_, c)| c)
            .collect();
        self.ses_mut().command_line = kept;
        self.command_selection = None;
        true
    }

    /// Grow or shrink the command-line selection by characters.
    fn extend_command_selection(&mut self, delta: isize) {
        let len = self.ses().command_line.chars().count();
        let head = match self.command_selection {
            Some((_, head)) => head,
            // No gesture yet: the caret sits at the end of what is typed, which
            // is the only place it can be — there is nowhere else to put it.
            None => len,
        };
        let next = head.saturating_add_signed(delta).min(len);
        self.set_command_selection_head(next);
    }

    fn set_command_selection_head(&mut self, head: usize) {
        let len = self.ses().command_line.chars().count();
        let head = head.min(len);
        let anchor = self.command_selection.map_or(len, |(a, _)| a).min(len);
        // Back at the anchor is no selection at all, rather than an empty one:
        // an empty selection would make Copy replace the clipboard with nothing.
        self.command_selection = (anchor != head).then_some((anchor, head));
    }

    /// The selected text on the command line, in reading order.
    pub(crate) fn command_selection_text(&self) -> Option<String> {
        let (a, b) = self.command_selection?;
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let line = &self.ses().command_line;
        Some(line.chars().skip(lo).take(hi - lo).collect())
    }

    /// The selected span as char offsets, low first, for the renderer.
    pub(crate) fn command_selection_span(&self) -> Option<(usize, usize)> {
        let (a, b) = self.command_selection?;
        Some(if a <= b { (a, b) } else { (b, a) })
    }

    /// Put the selection on the clipboard — the shell's, or the command line.
    fn copy_selection(&mut self) {
        // A deliberate selection first, whichever it is, then the whole command
        // line as the obvious fallback.
        let text = match self.shell_selection.zip(self.ses().hosted()) {
            Some((sel, sh)) => sel.text(sh),
            None => match self.command_selection_text() {
                Some(t) => t,
                None if !self.ses().command_line.is_empty() => self.ses().command_line.clone(),
                None => {
                    self.status = "nothing to copy".into();
                    return;
                }
            },
        };
        if text.trim().is_empty() {
            self.status = "nothing selected".into();
            return;
        }
        let n = text.chars().count();
        match dmac_core::clipboard::set_text(&text) {
            Ok(()) => self.status = format!("copied {n} characters"),
            // No local clipboard: ask the terminal for its own. Over SSH this
            // is not a fallback but the only correct answer — the system
            // clipboard here belongs to the wrong machine, and the terminal at
            // the far end is the one the user is looking at.
            Err(e) => match dmac_core::clipboard::osc52(&text) {
                Some(seq) => {
                    self.pending_terminal_write.push_str(&seq);
                    self.status = format!("copied {n} characters via the terminal");
                }
                None => self.status = format!("could not copy: {e}"),
            },
        }
    }

    /// Paste the clipboard wherever the keyboard is.
    fn paste_into_shell(&mut self) {
        let text = match dmac_core::clipboard::text() {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("nothing to paste: {e}");
                return;
            }
        };
        // On the panels there is no child to send bytes to, so it goes on the
        // command line — which is where you were about to type it anyway.
        // Refusing to paste unless a shell happens to be showing is not a
        // safety measure, it is just a paste that does not work.
        if self.ses().view != View::Shell {
            let n = text.chars().count();
            // Newlines would run as separate commands the moment Enter is
            // pressed; a paste is text, not a decision to run three things.
            let flat = text
                .lines()
                .map(str::trim_end)
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            self.ses_mut().command_line.push_str(&flat);
            self.ses_mut().focus = Focus::CommandLine;
            self.status = format!("pasted {n} characters");
            return;
        }

        let bracketed = self
            .ses()
            .hosted()
            .and_then(|sh| sh.with_screen(|s| s.bracketed_paste()))
            .unwrap_or(false);

        // Bracketed paste tells the shell "this is text, not typing", so a
        // pasted newline lands as a newline instead of running the line. When
        // the shell has not asked for it there is no way to say that, so the
        // trailing newline is dropped: the command arrives ready to run and the
        // user still has to press Enter. Pasting something that executes itself
        // is the one outcome worth engineering against.
        let mut payload = String::new();
        if bracketed {
            payload.push_str("\x1b[200~");
            payload.push_str(&text);
            payload.push_str("\x1b[201~");
        } else {
            payload.push_str(text.trim_end_matches(['\n', '\r']));
        }

        let waker = self.waker();
        let (cols, rows) = self.shell_size();
        match self.ses_mut().shell(cols, rows, waker) {
            Ok(sh) => match sh.write(payload.as_bytes()) {
                Ok(()) => {
                    let n = text.chars().count();
                    self.status = if bracketed {
                        format!("pasted {n} characters")
                    } else {
                        format!("pasted {n} characters — press Enter to run")
                    };
                }
                Err(e) => self.status = format!("paste: {e}"),
            },
            Err(e) => self.status = format!("paste: {e}"),
        }
    }

    /// Escape sequences owed to the real terminal, taken for writing.
    pub(crate) fn take_terminal_write(&mut self) -> Option<String> {
        (!self.pending_terminal_write.is_empty())
            .then(|| std::mem::take(&mut self.pending_terminal_write))
    }

    /// Where in the hosted screen a screen position falls, if it is over it.
    fn shell_cell_at(&self, column: u16, row: u16) -> Option<(u16, u16)> {
        let a = self.layout.shell;
        if self.ses().view != View::Shell || a.width == 0 || a.height == 0 {
            return None;
        }
        if column < a.x || column >= a.x + a.width || row < a.y || row >= a.y + a.height {
            return None;
        }
        Some((row - a.y, column - a.x))
    }

    /// Strip the frame off, or put it back.
    fn toggle_fullscreen(&mut self) {
        self.fullscreen = !self.fullscreen;
        self.status = if self.fullscreen {
            "full screen — F11 to bring the frame back".into()
        } else {
            String::new()
        };
    }

    /// Show the shell, or go back to the panels.
    fn toggle_shell(&mut self) {
        if self.ses().view == View::Shell {
            self.ses_mut().view = View::Panels;
            self.status.clear();
            return;
        }
        let (cols, rows) = self.shell_size();
        let waker = self.waker();
        match self.ses_mut().shell(cols, rows, waker) {
            Ok(_) => {
                // Norton's Ctrl-O landed you where the panel was, and that is
                // most of why it was worth pressing. Done on the way in rather
                // than on every navigation, so a shell you are not looking at
                // is never typed into behind your back.
                self.ses_mut().follow_panel_cwd();
                self.ses_mut().view = View::Shell;
                self.status.clear();
            }
            // A shell that will not start must say why. A blank pane the user
            // cannot explain is the worst possible outcome here.
            Err(e) => self.status = format!("shell: {e}"),
        }
    }

    /// Run what is on the command line in the session's shell.
    ///
    /// Switches to the shell view, because a command whose output you cannot
    /// see has not really run as far as the user is concerned.
    fn run_command(&mut self) {
        let line = self.ses().command_line.trim().to_string();
        if line.is_empty() {
            return;
        }

        let (cols, rows) = self.shell_size();
        let waker = self.waker();
        match self.ses_mut().shell(cols, rows, waker) {
            Ok(shell) => match shell.run(&line) {
                Ok(()) => {
                    self.ses_mut().command_line.clear();
                    self.ses_mut().view = View::Shell;
                    self.status.clear();
                }
                Err(e) => self.status = format!("shell: {e}"),
            },
            Err(e) => self.status = format!("shell: {e}"),
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
        self.touch_sessions();
        self.quick_search.clear();
        // Close an overlay that belonged to the session we left — but not the
        // rail, which is how you got here and where you still are. Clobbering it
        // silently dropped the keyboard back to the panels, so the next
        // keystrokes landed on files instead of on the session list.
        if !matches!(self.mode, Mode::Rail { .. } | Mode::Prompt { .. }) {
            self.mode = Mode::Normal;
        }
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
            Mode::Rail { selected } => return self.rail_key(k, selected),
            Mode::Prompt { intent } => return self.prompt_key(k, intent),
            Mode::Utilities { selected } => return self.utilities_key(k, selected),
            Mode::Normal => {}
        }

        // While the shell is showing it owns the keyboard, or half the keys a
        // shell needs would be eaten by the file manager. Two bindings are
        // reserved: the one that gets you back out, and the one that changes
        // how the screen is drawn — a display mode that stopped working in one
        // view would be a worse surprise than a hosted program losing F11.
        if self.ses().view == View::Shell {
            match keymap::resolve(k, self.ses().focus) {
                Some(Action::ToggleShell) => {
                    self.toggle_shell();
                    return;
                }
                Some(Action::ToggleFullscreen) => {
                    self.toggle_fullscreen();
                    return;
                }
                Some(a @ (Action::ClipboardCopy | Action::ClipboardPaste)) => {
                    self.handle(a);
                    return;
                }
                // Sessions stay reachable from inside a shell. Being able to
                // start something long-running and then leave it to look at
                // another session is most of what several sessions are for.
                Some(
                    a @ (Action::ToggleRail | Action::CycleSession(_) | Action::SwitchSession(_)),
                ) => {
                    self.handle(a);
                    return;
                }
                // Ctrl-Shift-U, never plain Ctrl-U: in a shell that is
                // readline's kill-line, and a file manager that swallowed it
                // would break every shell it hosts.
                Some(Action::UtilitiesMenu)
                    if k.modifiers
                        .intersects(KeyModifiers::SHIFT | KeyModifiers::SUPER) =>
                {
                    self.handle(Action::UtilitiesMenu);
                    return;
                }
                _ => {}
            }
            self.send_to_shell(k);
            return;
        }

        if let Some(action) = keymap::resolve(k, self.ses().focus) {
            self.handle(action);
        }
    }

    /// Driving the utilities menu.
    fn utilities_key(&mut self, k: KeyEvent, selected: usize) {
        use crate::utilities::Utility;
        let items = crate::utilities::items();
        match k.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Up => {
                if let Some(i) = crate::ui::menu::next_selectable(&items, selected, -1) {
                    self.mode = Mode::Utilities { selected: i };
                }
            }
            KeyCode::Down => {
                if let Some(i) = crate::ui::menu::next_selectable(&items, selected, 1) {
                    self.mode = Mode::Utilities { selected: i };
                }
            }
            KeyCode::Enter => {
                if let Some(u) = Utility::at(selected) {
                    self.run_utility(u);
                }
            }
            // The accelerator shown in the hint column. A menu that lists its
            // shortcuts and does not answer to them is worse than one that
            // lists none.
            KeyCode::Char(c) => {
                if let Some(u) = Utility::from_key(c.to_ascii_lowercase()) {
                    self.run_utility(u);
                }
            }
            _ => {}
        }
    }

    /// Apply a utility to the command line, and get out of the menu's way.
    fn run_utility(&mut self, u: crate::utilities::Utility) {
        use crate::utilities::{Context, Outcome};

        let ses = self.ses();
        let this = Self::idx(ses.active);
        let other = 1 - this;
        let panel = &ses.panels[this];
        let base = ses.cwd[this].clone();
        let names: Vec<String> = panel.operands().iter().map(|e| e.name.clone()).collect();
        let paths: Vec<String> = names
            .iter()
            .map(|n| match base.join(n) {
                Some(p) => p.to_string(),
                // A name the VFS refuses to join is shown as itself rather than
                // dropped: a shorter list than the one you selected is the kind
                // of wrong that gets noticed after the command has run.
                None => n.clone(),
            })
            .collect();

        // What the utilities act on: the shell's selection when there is one,
        // and the command line otherwise. Reachable from the shell view, this
        // is the difference between a useful menu and one whose transforms all
        // report an empty command line.
        let selection = self
            .shell_selection
            .zip(ses.hosted())
            .map(|(sel, sh)| sel.text(sh));

        let cx = Context {
            line: &ses.command_line,
            selection: selection.as_deref(),
            this_path: ses.cwd[this].to_string(),
            other_path: ses.cwd[other].to_string(),
            selected: names,
            selected_paths: paths,
        };

        match crate::utilities::run(u, &cx) {
            Outcome::Insert(text) => {
                let line = &mut self.ses_mut().command_line;
                // A separating space, but only where one is wanted: after
                // `cd ` there is already one, and at the start there is nothing
                // to separate from.
                if !line.is_empty() && !line.ends_with(' ') {
                    line.push(' ');
                }
                line.push_str(&text);
                self.after_utility(u);
            }
            Outcome::Replace(text) => {
                self.ses_mut().command_line = text;
                self.after_utility(u);
            }
            // Left open on purpose: the menu is still there to pick something
            // else, which is what you want when you picked the wrong entry.
            Outcome::Nothing(why) => self.status = why.to_string(),
        }
    }

    /// Close the menu and put the keyboard where the text landed.
    fn after_utility(&mut self, u: crate::utilities::Utility) {
        self.mode = Mode::Normal;
        self.ses_mut().focus = Focus::CommandLine;
        self.status = format!("{} — Enter to run, Ctrl-Y to clear", u.label());
        self.touch_sessions();
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

    fn send_to_shell(&mut self, k: KeyEvent) {
        let Some(bytes) = crate::ui::shell::encode(k) else {
            return;
        };
        let (cols, rows) = self.shell_size();
        let waker = self.waker();
        match self.ses_mut().shell(cols, rows, waker) {
            Ok(shell) => {
                if let Err(e) = shell.write(&bytes) {
                    // The shell exited under us. Say so and go back to the
                    // panels rather than swallowing keys into a dead process.
                    self.status = format!("shell: {e}");
                    self.ses_mut().view = View::Panels;
                }
            }
            Err(e) => {
                self.status = format!("shell: {e}");
                self.ses_mut().view = View::Panels;
            }
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

        // The shell owns the pointer while it is showing: there are no rows to
        // click there, only text to select.
        if self.ses().view == View::Shell && self.shell_mouse(m) {
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
                            p == id && i == index && now.duration_since(t) < DOUBLE_CLICK
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

    /// Selecting text over the hosted shell. Returns whether the event was ours.
    fn shell_mouse(&mut self, m: MouseEvent) -> bool {
        let Some((row, col)) = self.shell_cell_at(m.column, m.row) else {
            return false;
        };
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // A second click inside the current selection takes the word
                // under it, the way every terminal has since X11.
                let double = self
                    .last_shell_click
                    .is_some_and(|(t, r, c)| t.elapsed() < DOUBLE_CLICK && (r, c) == (row, col));
                let mut sel = crate::ui::shell::Selection::new(row, col);
                if double && let Some(sh) = self.ses().hosted() {
                    sel.expand_to_word(sh);
                    self.shell_selection = Some(sel);
                    self.selecting = false;
                    self.copy_selection();
                    self.last_shell_click = None;
                    return true;
                }
                self.last_shell_click = Some((std::time::Instant::now(), row, col));
                self.shell_selection = Some(sel);
                self.selecting = true;
                true
            }
            MouseEventKind::Drag(MouseButton::Left) if self.selecting => {
                if let Some(sel) = self.shell_selection.as_mut() {
                    sel.extend_to(row, col);
                }
                true
            }
            MouseEventKind::Up(MouseButton::Left) if self.selecting => {
                self.selecting = false;
                match self.shell_selection {
                    // A click that never moved is a click, not a selection.
                    // Copying it would silently replace the clipboard with
                    // nothing, which is a way to lose work.
                    Some(sel) if sel.is_empty() => self.shell_selection = None,
                    Some(_) => self.copy_selection(),
                    None => {}
                }
                true
            }
            _ => false,
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
                    self.touch_sessions();
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
            self.touch_sessions();
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
pub async fn run(mut start: Startup) -> anyhow::Result<()> {
    let cursor = start.cursor;
    let restored = start.restored.take();
    let mut guard = TerminalGuard::enter(cursor)?;

    let (update_tx, mut update_rx) = mpsc::unbounded_channel();
    let mut app = App::new(start, update_tx);

    // Replace the freshly-made session set with what was on disk, if anything,
    // and list every panel of every session — they are all live, so they all
    // need their contents.
    if let Some((sessions, clean_exit)) = restored {
        let count = sessions.len();
        app.sessions = sessions;
        if !clean_exit {
            // Never let a crash pass silently: a layout that looks subtly stale
            // with no explanation is worse than one that says what happened.
            app.status =
                "previous run did not exit cleanly — sessions restored from the last save".into();
        }
        for i in 0..count {
            app.reload_session(i, PanelId::Left);
            app.reload_session(i, PanelId::Right);
        }
    } else {
        app.reload(PanelId::Left);
        app.reload(PanelId::Right);
    }

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

    // Nothing of ours is running yet, so the terminal going away — the window
    // closed, the session logged out, a `kill` — has to run the same teardown
    // F10 does. Without this the hosted tree is simply abandoned, which is how
    // an agent survives to hold a conversation the next run will ask for.
    #[cfg(unix)]
    let mut signals = {
        use tokio::signal::unix::{SignalKind, signal};
        let mut set = Vec::new();
        for kind in [
            SignalKind::hangup(),
            SignalKind::terminate(),
            SignalKind::interrupt(),
        ] {
            if let Ok(s) = signal(kind) {
                set.push(s);
            }
        }
        set
    };

    let mut first_frame = true;

    loop {
        app.before_frame();
        guard.terminal().draw(|f| ui::draw(f, &mut app))?;
        app.sync_shell_size();
        if first_frame {
            first_frame = false;
            // After the frame, so a cold start still shows something inside its
            // budget and the spawning happens where the user can watch it.
            app.reattach_agents();
        }
        // After the frame, never during it: stdout is shared with ratatui and
        // interleaving with a half-written frame corrupts both.
        if let Some(seq) = app.take_terminal_write() {
            use std::io::Write;
            let mut out = std::io::stdout();
            let _ = out.write_all(seq.as_bytes());
            let _ = out.flush();
        }

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
        // Writing sessions is debounced, so it contributes a deadline like
        // everything else rather than a timer of its own.
        let save_due = app.flush_sessions();
        let wake = [
            app.splash_until,
            app.screensaver.deadline(),
            app.cursor_deadline(),
            app.shell_deadline(),
            save_due,
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
            // Any of the ways a terminal tells an application to go away.
            Some(()) = wait_for_signal(&mut signals) => {
                app.request_quit();
            }
            else => break,
        }

        if app.should_quit {
            break;
        }
    }

    app.save_on_exit();
    Ok(())
}

/// Resolve when any of these signals arrives.
///
/// A helper because `tokio::select!` needs one future, and which signals exist
/// is a platform question that should not be spelled out inside the loop.
#[cfg(unix)]
async fn wait_for_signal(set: &mut [tokio::signal::unix::Signal]) -> Option<()> {
    if set.is_empty() {
        return std::future::pending().await;
    }
    let mut futures: Vec<_> = set.iter_mut().map(|s| Box::pin(s.recv())).collect();
    let (result, _, _) = futures::future::select_all(futures.iter_mut()).await;
    result.map(|_| ())
}

#[cfg(not(unix))]
async fn wait_for_signal(_set: &mut [()]) -> Option<()> {
    std::future::pending().await
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
            Startup {
                session_name: "test".to_string(),
                left: VfsPath::local("/left"),
                right: VfsPath::local("/right"),
                // Tests must never have a screensaver appear mid-assertion, and
                // no splash: it would swallow the first key of every one.
                screensaver: ScreensaverConfig {
                    enabled: false,
                    ..Default::default()
                },
                splash: false,
                cursor: CursorStyle::default(),
                // Tests never touch the real session file.
                store: None,
                restored: None,
            },
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

    /// Full screen takes the frame off and leaves the contents. If a border
    /// survives it, the mode has not done the one thing it is for.
    #[test]
    fn full_screen_removes_every_border_and_the_f_key_bar() {
        let mut app = fixture();
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();

        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let framed: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(framed.contains('│'), "the normal frame should have borders");

        app.handle(Action::ToggleFullscreen);
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer();
        let bare: String = buf.content().iter().map(|c| c.symbol()).collect();

        // Corners only, plus the columns a box would occupy. A filename can
        // legitimately contain '│' — the fixture has one — so the bare glyph is
        // not evidence of a border, and asserting on it tests the fixture.
        for ch in ['┌', '┐', '└', '┘'] {
            assert!(
                !bare.contains(ch),
                "full screen still draws a box corner {ch:?}"
            );
        }
        for y in 0..23 {
            for x in [0u16, 40, 79] {
                let c = buf[(x, y)].symbol();
                assert_ne!(c, "│", "a panel edge survived at {x},{y}");
            }
        }
        let last: String = (0..80).map(|x| buf[(x, 23)].symbol().to_string()).collect();
        assert!(
            !last.contains("Copy"),
            "the F-key bar should be gone: {last:?}"
        );
    }

    /// Black, not Norton blue: full screen is for looking at contents, and blue
    /// reads as a surface holding something.
    #[test]
    fn full_screen_paints_the_background_black() {
        let mut app = fixture();
        app.handle(Action::ToggleFullscreen);
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();

        let buf = term.backend().buffer();
        let blue = buf
            .content()
            .iter()
            .filter(|c| c.style().bg == Some(ratatui::style::Color::Blue))
            .count();
        assert_eq!(blue, 0, "{blue} cells are still Norton blue");
    }

    /// The row the F-key bar gave up has to go to the listing, or the mode costs
    /// a border and buys nothing.
    #[test]
    fn full_screen_gives_its_rows_to_the_listing() {
        let mut app = fixture();
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let framed = app.layout.panels[0].height;

        app.handle(Action::ToggleFullscreen);
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let bare = app.layout.panels[0].height;

        assert!(
            bare > framed,
            "full screen showed {bare} rows against {framed} framed"
        );
    }

    #[test]
    fn f11_is_full_screen_and_toggles_back() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        assert_eq!(
            keymap::resolve(
                KeyEvent::new(KeyCode::F(11), KeyModifiers::NONE),
                Focus::Panel
            ),
            Some(Action::ToggleFullscreen)
        );
        let mut app = fixture();
        assert!(!app.fullscreen);
        app.handle(Action::ToggleFullscreen);
        assert!(app.fullscreen);
        app.handle(Action::ToggleFullscreen);
        assert!(!app.fullscreen);
    }

    /// The collapsed strip is a hint and goes with the rest of the frame, but a
    /// rail you opened on purpose is contents and must still work.
    #[test]
    fn the_rail_still_opens_in_full_screen() {
        let mut app = fixture();
        app.handle(Action::ToggleFullscreen);
        app.handle(Action::ToggleRail);
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        assert!(
            app.layout.rail.width > 0,
            "the rail vanished in full screen"
        );
    }

    /// A hosted CLI you cannot see the cursor of is a CLI you cannot tell is
    /// waiting for you. The style is re-asserted every frame that shows one, so
    /// this predicate is also what makes it blink.
    #[cfg(unix)]
    #[test]
    fn the_hosted_shell_gets_a_visible_cursor() {
        let mut app = fixture();
        assert!(
            !app.wants_text_cursor(),
            "a panel with the keyboard must have no cursor"
        );

        app.handle(Action::ToggleShell);
        assert_eq!(app.ses().view, View::Shell);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !app.wants_text_cursor() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            app.wants_text_cursor(),
            "the hosted shell never showed a cursor"
        );
        assert!(
            app.shows_text_cursor(),
            "and its style is never re-asserted"
        );
    }

    /// A terminal program only learns it was resized because the terminal tells
    /// it. Doing that lazily is why a hosted `vim` kept the old geometry until
    /// the next keystroke.
    #[cfg(unix)]
    #[test]
    fn resizing_reaches_the_hosted_shell_without_a_keystroke() {
        let mut app = fixture();
        app.handle(Action::ToggleShell);

        let mut small = Terminal::new(TestBackend::new(80, 24)).unwrap();
        small.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        app.sync_shell_size();
        let before = app.ses().hosted().map(|s| s.size()).expect("a shell");

        let mut big = Terminal::new(TestBackend::new(140, 40)).unwrap();
        big.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        app.sync_shell_size();
        let after = app.ses().hosted().map(|s| s.size()).expect("a shell");

        assert_ne!(before, after, "the child was never told the window changed");
        assert!(
            after.0 > before.0 && after.1 > before.1,
            "expected a bigger pty than {before:?}, got {after:?}"
        );
    }

    /// Ctrl-Shift-U has to reach the menu from wherever the user is, including
    /// from inside a hosted shell — a utilities menu you can only open from one
    /// view is one you stop reaching for.
    #[test]
    fn ctrl_shift_u_opens_the_utilities_menu_everywhere() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let ctrl_shift_u = KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        for focus in [Focus::Panel, Focus::CommandLine] {
            assert_eq!(
                keymap::resolve(ctrl_shift_u, focus),
                Some(Action::UtilitiesMenu),
                "{focus:?}"
            );
        }

        let mut app = fixture();
        app.on_key(ctrl_shift_u);
        assert!(
            matches!(app.mode, Mode::Utilities { .. }),
            "from the panels"
        );

        let mut app = fixture();
        app.handle(Action::ToggleShell);
        assert_eq!(app.ses().view, View::Shell);
        app.on_key(ctrl_shift_u);
        assert!(
            matches!(app.mode, Mode::Utilities { .. }),
            "the shell swallowed it"
        );
    }

    /// Plain Ctrl-U is readline's kill-line. Swallowing it would break every
    /// shell the file manager hosts, so in the shell view it goes to the child.
    #[test]
    fn plain_ctrl_u_still_belongs_to_the_shell() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = fixture();
        app.handle(Action::ToggleShell);
        app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(app.mode, Mode::Normal, "the menu stole the shell's Ctrl-U");
        assert_eq!(app.ses().view, View::Shell);
    }

    /// Ctrl-Tab and Ctrl-Shift-Tab move between sessions, in both the spellings
    /// terminals use for the shifted one.
    #[test]
    fn ctrl_tab_cycles_sessions_in_either_encoding() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let ctrl = KeyModifiers::CONTROL;
        let ctrl_shift = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert_eq!(
            keymap::resolve(KeyEvent::new(KeyCode::Tab, ctrl), Focus::Panel),
            Some(Action::CycleSession(1))
        );
        for shifted in [
            KeyEvent::new(KeyCode::Tab, ctrl_shift),
            KeyEvent::new(KeyCode::BackTab, ctrl_shift),
            KeyEvent::new(KeyCode::BackTab, ctrl),
        ] {
            assert_eq!(
                keymap::resolve(shifted, Focus::Panel),
                Some(Action::CycleSession(-1)),
                "{shifted:?}"
            );
        }
        // Bare Shift-Tab still opens the rail; the Ctrl arms must not shadow it.
        assert_eq!(
            keymap::resolve(
                KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
                Focus::Panel
            ),
            Some(Action::ToggleRail)
        );
    }

    /// Switching sessions from inside a shell is most of what several sessions
    /// are for: start something long-running, then go and look at another one.
    // A runtime, because leaving a session re-lists the one arrived at, and
    // listings are spawned tasks.
    #[tokio::test]
    async fn sessions_are_reachable_from_inside_the_shell() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = fixture();
        app.handle(Action::NewSession);
        app.mode = Mode::Normal;
        let before = app.sessions.current().id;

        app.handle(Action::ToggleShell);
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::CONTROL));
        assert_ne!(
            app.sessions.current().id,
            before,
            "Ctrl-Tab did not leave the session"
        );
    }

    /// A selection you cannot see is one you cannot trust, and this one decides
    /// what Copy puts on the clipboard.
    #[test]
    fn the_command_line_selection_is_drawn_inverted() {
        let mut app = fixture();
        app.ses_mut().focus = Focus::CommandLine;
        for c in "echo ciao".chars() {
            app.handle(Action::CommandChar(c));
        }
        for _ in 0..4 {
            app.handle(Action::ExtendCommandSelection(-1));
        }
        assert_eq!(app.command_selection_text().as_deref(), Some("ciao"));

        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer();
        let inverted: String = (0..80)
            .map(|x| buf[(x, 22)].clone())
            .filter(|c| {
                c.style()
                    .add_modifier
                    .contains(ratatui::style::Modifier::REVERSED)
            })
            .map(|c| c.symbol().to_string())
            .collect();
        assert_eq!(inverted, "ciao", "the highlight is on the wrong characters");
    }

    /// Shift+Left then Shift+Right lands back where it started, and back at the
    /// anchor is no selection rather than an empty one — an empty selection
    /// would make Copy replace the clipboard with nothing.
    #[test]
    fn coming_back_to_the_anchor_clears_the_selection() {
        let mut app = fixture();
        app.ses_mut().focus = Focus::CommandLine;
        for c in "abc".chars() {
            app.handle(Action::CommandChar(c));
        }
        app.handle(Action::ExtendCommandSelection(-1));
        assert_eq!(app.command_selection_text().as_deref(), Some("c"));
        app.handle(Action::ExtendCommandSelection(1));
        assert!(app.command_selection.is_none());
        assert!(app.command_selection_text().is_none());
    }

    #[test]
    fn selecting_to_the_start_takes_the_whole_line() {
        let mut app = fixture();
        app.ses_mut().focus = Focus::CommandLine;
        for c in "ls -la".chars() {
            app.handle(Action::CommandChar(c));
        }
        app.handle(Action::ExtendCommandSelectionToStart);
        assert_eq!(app.command_selection_text().as_deref(), Some("ls -la"));
    }

    /// Typing over a selection replaces it, as in every editor. Appending past
    /// it would leave a highlight sitting over text that is no longer selected.
    #[test]
    fn typing_replaces_what_was_selected() {
        let mut app = fixture();
        app.ses_mut().focus = Focus::CommandLine;
        for c in "echo ciao".chars() {
            app.handle(Action::CommandChar(c));
        }
        for _ in 0..4 {
            app.handle(Action::ExtendCommandSelection(-1));
        }
        app.handle(Action::CommandChar('x'));
        assert_eq!(app.ses().command_line, "echo x");
        assert!(app.command_selection.is_none());
    }

    /// Backspace on a selection deletes the selection, not the character before
    /// it — deleting one character out of five the user asked to remove is the
    /// kind of wrong that is only noticed afterwards.
    #[test]
    fn backspace_deletes_the_selection() {
        let mut app = fixture();
        app.ses_mut().focus = Focus::CommandLine;
        for c in "rm -rf tmp".chars() {
            app.handle(Action::CommandChar(c));
        }
        for _ in 0..3 {
            app.handle(Action::ExtendCommandSelection(-1));
        }
        app.handle(Action::CommandBackspace);
        assert_eq!(app.ses().command_line, "rm -rf ");
    }

    /// Char offsets, not byte offsets: slicing a multi-byte character down the
    /// middle panics rather than misbehaving quietly.
    #[test]
    fn selection_offsets_survive_multibyte_text() {
        let mut app = fixture();
        app.ses_mut().focus = Focus::CommandLine;
        for c in "echo 日本語".chars() {
            app.handle(Action::CommandChar(c));
        }
        for _ in 0..3 {
            app.handle(Action::ExtendCommandSelection(-1));
        }
        assert_eq!(app.command_selection_text().as_deref(), Some("日本語"));
        app.handle(Action::CommandBackspace);
        assert_eq!(app.ses().command_line, "echo ");
    }

    /// Shift+Left has to mean the command line, not the panel selection.
    #[test]
    fn shift_left_and_right_are_the_command_line_selection() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        for (code, want) in [
            (KeyCode::Left, Action::ExtendCommandSelection(-1)),
            (KeyCode::Right, Action::ExtendCommandSelection(1)),
        ] {
            assert_eq!(
                keymap::resolve(KeyEvent::new(code, KeyModifiers::SHIFT), Focus::CommandLine),
                Some(want)
            );
        }
        // Shift+Home still belongs to the panel when a panel has the keyboard.
        assert_eq!(
            keymap::resolve(
                KeyEvent::new(KeyCode::Home, KeyModifiers::SHIFT),
                Focus::Panel
            ),
            Some(Action::ExtendSelectionToTop)
        );
    }
}
