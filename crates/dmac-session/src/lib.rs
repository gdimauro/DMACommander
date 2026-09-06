//! Named sessions: several live workspaces at once.
//!
//! Owned by the `session-engineer` agent.
//!
//! A session is not "which directories were open" — it is *what you were doing*.
//! It owns its panels, its focus, its command line, and (later) the processes it
//! hosts and the external windows it launched.
//!
//! The important property is that sessions are **live, not swapped**. Switching
//! does not save one and load another; every session stays in memory with its
//! listings intact, so switching is instant and nothing reloads. That is what
//! makes the session rail usable as a window switcher.
// Tests assert; `unwrap`/`expect` there are how a failure is reported.
// In non-test code the workspace lints still forbid them.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod store;

pub mod agent;
use dmac_core::{Panel, PanelId};
use dmac_pty::Hosted;
use dmac_vfs::VfsPath;

/// What a session is showing.
///
/// The shell is not a separate window: it is an alternative view of the same
/// session, exactly as Ctrl-O in Norton Commander revealed the shell underneath
/// the panels. Which view you were in is session state, so switching sessions
/// and coming back puts you where you were.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Panels,
    Shell,
}

/// Where the keyboard is within a session. Two places, never ambiguous.
///
/// Session state rather than application state: each session remembers whether
/// you were typing a command or moving around, and switching back restores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Panel,
    CommandLine,
}

/// Stable within a run. Not persisted — the on-disk identity is the name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(pub u64);

/// The colour dot in the rail. Assigned on creation and cycled, so two adjacent
/// sessions are never the same colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionColor {
    Cyan,
    Green,
    Yellow,
    Magenta,
    Blue,
    Red,
}

impl SessionColor {
    const ALL: [SessionColor; 6] = [
        SessionColor::Cyan,
        SessionColor::Green,
        SessionColor::Yellow,
        SessionColor::Magenta,
        SessionColor::Blue,
        SessionColor::Red,
    ];

    pub(crate) fn nth(i: usize) -> Self {
        Self::ALL[i % Self::ALL.len()]
    }
}

/// One live workspace.
pub struct Session {
    pub id: SessionId,
    pub name: String,
    pub color: SessionColor,
    pub panels: [Panel; 2],
    pub cwd: [VfsPath; 2],
    /// Bumped on every navigation so results from an abandoned listing are
    /// dropped instead of painted into the wrong directory.
    pub generation: [u64; 2],
    pub active: PanelId,
    pub focus: Focus,
    pub command_line: String,
    pub panels_hidden: bool,
    pub view: View,
    /// The session's shell, spawned on first use rather than at startup: most
    /// sessions never need one, and paying for a process per session up front
    /// would be a cost for nothing.
    pub shell: Option<Hosted>,
    /// Where the hosted shell was last sent. Compared against the panel's
    /// directory so a `cd` is written when it would change something, and not
    /// on every visit — re-entering a directory you are already in still costs
    /// a line of scrollback and a wasted prompt.
    pub shell_cwd: Option<VfsPath>,
    /// The conversation a hosted agent in this session belongs to.
    ///
    /// Generated once and then kept for the life of the session, because that
    /// is the whole point: come back tomorrow and `claude` rejoins the
    /// conversation you left, instead of starting a new one next to it.
    pub conversation: Option<String>,
    /// Whether this session's panels have been listed yet.
    ///
    /// Restored sessions start `false`: listing every panel of every session at
    /// startup means a directory walk per panel before the first frame, which
    /// is a cost that grows with how many sessions you keep and buys nothing —
    /// you can only look at one of them. They are listed when first visited,
    /// and stay in memory afterwards, so switching is still instant.
    pub loaded: bool,
    /// An agent that was running when this session was last saved, waiting to
    /// be started again. Cleared once it has been.
    pub reattach: Option<String>,
    /// The agent's whole command line as found running at save time, recorded
    /// for the store so the next run can put back what was there, not an
    /// approximation of it.
    pub agent: Option<String>,
    pub last_used: std::time::Instant,
}

impl Session {
    pub fn new(id: SessionId, name: impl Into<String>, left: VfsPath, right: VfsPath) -> Self {
        Self {
            id,
            name: name.into(),
            color: SessionColor::nth(id.0 as usize),
            panels: [Panel::new(left.display()), Panel::new(right.display())],
            cwd: [left, right],
            generation: [0, 0],
            active: PanelId::Left,
            focus: Focus::Panel,
            command_line: String::new(),
            panels_hidden: false,
            view: View::Panels,
            shell: None,
            shell_cwd: None,
            conversation: None,
            // A session made now is listed by whoever made it.
            loaded: true,
            reattach: None,
            agent: None,
            last_used: std::time::Instant::now(),
        }
    }

    pub fn index_of(id: PanelId) -> usize {
        match id {
            PanelId::Left => 0,
            PanelId::Right => 1,
        }
    }

    pub fn panel(&self, id: PanelId) -> &Panel {
        &self.panels[Self::index_of(id)]
    }

    pub fn panel_mut(&mut self, id: PanelId) -> &mut Panel {
        &mut self.panels[Self::index_of(id)]
    }

    pub fn active_panel(&self) -> &Panel {
        self.panel(self.active)
    }

    pub fn active_panel_mut(&mut self) -> &mut Panel {
        let id = self.active;
        self.panel_mut(id)
    }

    pub fn active_cwd(&self) -> &VfsPath {
        &self.cwd[Self::index_of(self.active)]
    }

    /// The session's shell, starting it if this is the first time.
    ///
    /// Errors are returned rather than swallowed: a shell that failed to start
    /// must say so, because the alternative is a blank pane the user cannot
    /// explain.
    /// The session's shell, started on first use.
    ///
    /// `waker` is handed to a shell that is being created, so its output can
    /// ask to be repainted. A caller that does not pass one gets a pane that
    /// only updates when something else happens to redraw it.
    pub fn shell(
        &mut self,
        cols: u16,
        rows: u16,
        waker: Option<dmac_pty::Waker>,
    ) -> dmac_pty::Result<&mut Hosted> {
        // A shell whose process has gone is replaced, not reused: typing into a
        // dead shell forever is worse than starting a new one.
        if self.shell.as_ref().is_some_and(|s| s.finished()) {
            self.shell = None;
        }
        if self.shell.is_none() {
            let cwd = self.cwd[Self::index_of(self.active)].clone();
            let dir = cwd.is_local().then(|| cwd.as_path().to_path_buf());
            let env = self.agent_environment();
            self.shell = Some(Hosted::shell(dir.as_deref(), cols, rows, waker, &env)?);
            // It started there, so it is already there: recording it is what
            // keeps the first Ctrl-O from writing a `cd` to where we already are.
            self.shell_cwd = cwd.is_local().then(|| cwd.clone());
        }
        match self.shell.as_mut() {
            Some(s) => {
                s.resize(cols, rows)?;
                Ok(s)
            }
            None => Err(dmac_pty::PtyError::Gone),
        }
    }

    /// Whether a shell has been started and is still alive.
    /// The conversation id for this session, made on first ask.
    ///
    /// A UUID because that is what `claude --session-id` takes, and because it
    /// is the same identifier `ps` will then show — which is how you tell, from
    /// outside, which conversation a running process belongs to.
    pub fn conversation_id(&mut self) -> &str {
        self.conversation
            .get_or_insert_with(dmac_core::tools::uuid_v4)
    }

    /// Environment for a shell started in this session: the ids, in plain
    /// sight. Nothing on `PATH` — what a command resolves to in the user's
    /// shell stays the user's business.
    fn agent_environment(&mut self) -> Vec<(String, String)> {
        let (id, name) = (self.id.0.to_string(), self.name.clone());
        let conversation = self.conversation_id().to_string();
        agent::environment(&id, &name, &conversation)
    }

    /// The attached agent running in this session's shell right now, if any.
    pub fn running_agent(&mut self) -> Option<String> {
        let commands = self.shell.as_mut()?.running_commands();
        agent::running_agent(&commands)
    }

    /// The session's shell if it has one, without starting one.
    pub fn hosted(&self) -> Option<&Hosted> {
        self.shell.as_ref()
    }

    /// Send the hosted shell to the active panel's directory.
    ///
    /// Norton's Ctrl-O did this, and it is most of why the key was worth
    /// pressing: the panels are how you navigate, and a shell that ignores
    /// where you navigated to is a shell you have to re-navigate by hand.
    ///
    /// Silent when it cannot be done — a remote or in-archive directory has no
    /// meaning to a shell, and a shell running something must not be typed
    /// into. Neither is an error worth interrupting anyone about.
    pub fn follow_panel_cwd(&mut self) {
        let cwd = self.cwd[Self::index_of(self.active)].clone();
        if !cwd.is_local() || self.shell_cwd.as_ref() == Some(&cwd) {
            return;
        }
        let Some(shell) = self.shell.as_mut() else {
            return;
        };
        if matches!(shell.cd(cwd.as_path()), Ok(true)) {
            self.shell_cwd = Some(cwd);
        }
    }

    pub fn shell_running(&self) -> bool {
        self.shell.as_ref().is_some_and(|s| !s.finished())
    }

    /// One line for the rail: what this session is looking at.
    pub fn subtitle(&self) -> String {
        abbreviate_home(&self.cwd[Self::index_of(self.active)].display())
    }
}

/// Every live session, and which one is on screen.
///
/// There is always at least one: closing the last session is refused rather
/// than leaving the application with nothing to render.
pub struct SessionManager {
    sessions: Vec<Session>,
    current: usize,
    next_id: u64,
    /// Every directory the panels have landed in, across every session. It
    /// lives here rather than on a `Session` because two of its three views
    /// span all of them, and a per-session list would have to be re-merged on
    /// every keystroke of the filter.
    pub history: dmac_core::history::History,
    /// How wide the session rail is drawn. It lives here, with the rest of what
    /// comes back when you reopen the application, rather than in the TUI:
    /// a width the user set by hand and lost on restart is a width they would
    /// have to set again every morning.
    pub rail: RailWidths,
}

/// The two widths of the session rail, in columns.
///
/// Two and not one, because the resting strip and the opened list are answers
/// to different questions — "how many sessions are there" and "which one do I
/// want" — and a single width would make one of them wrong. A resting strip
/// widened to `DETAILED_FROM` or more shows names, which is how you ask to see
/// the sessions all the time without keeping the list open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RailWidths {
    pub collapsed: u16,
    pub expanded: u16,
}

impl Default for RailWidths {
    fn default() -> Self {
        Self {
            collapsed: 3,
            expanded: 22,
        }
    }
}

impl SessionManager {
    pub fn new(name: impl Into<String>, left: VfsPath, right: VfsPath) -> Self {
        let first = Session::new(SessionId(0), name, left, right);
        Self {
            sessions: vec![first],
            current: 0,
            next_id: 1,
            history: dmac_core::history::History::default(),
            rail: RailWidths::default(),
        }
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        false // there is always at least one
    }

    pub fn current_index(&self) -> usize {
        self.current
    }

    pub fn current(&self) -> &Session {
        // `current` is kept in range by every mutator, and there is always at
        // least one session; the fallback keeps the lint rules satisfied without
        // an unwrap on a path that cannot be reached.
        self.sessions.get(self.current).unwrap_or(&self.sessions[0])
    }

    pub fn current_mut(&mut self) -> &mut Session {
        let i = self.current.min(self.sessions.len() - 1);
        &mut self.sessions[i]
    }

    pub fn all(&self) -> &[Session] {
        &self.sessions
    }

    pub fn get(&self, index: usize) -> Option<&Session> {
        self.sessions.get(index)
    }

    /// Where a session lives now. Positions shift as sessions are closed, so
    /// anything holding on to a session across time must go through its id.
    pub fn index_of_id(&self, id: SessionId) -> Option<usize> {
        self.sessions.iter().position(|s| s.id == id)
    }

    /// Mutable access by position, for delivering work to a session that is not
    /// the one on screen.
    /// Ask every hosted process to go away, then insist, once.
    ///
    /// Two passes with one grace period between them, rather than a grace
    /// period per session: quitting with four shells open should not take four
    /// times as long as quitting with one.
    ///
    /// Nothing DMACommander started may outlive it. A hosted `claude` that
    /// survives goes on holding the session it opened, and the next run is told
    /// that session is already in use — which is not an inconvenience, it is
    /// the user locked out of their own work.
    pub fn shutdown(&mut self, grace: std::time::Duration) {
        for s in &mut self.sessions {
            if let Some(sh) = s.shell.as_mut() {
                sh.hangup();
            }
        }
        for s in &mut self.sessions {
            if let Some(sh) = s.shell.as_mut() {
                sh.terminate(grace);
            }
        }
    }

    /// A session by position, without making it current.
    ///
    /// The read-only half of `at_mut`, which existed alone: everything that
    /// only wants to *look* at another session had to take a mutable borrow to
    /// do it, and a borrow that says "I will change this" when it will not is a
    /// borrow that eventually blocks something legitimate.
    pub fn at(&self, index: usize) -> &Session {
        self.sessions
            .get(index.min(self.sessions.len().saturating_sub(1)))
            .unwrap_or_else(|| self.current())
    }

    pub fn at_mut(&mut self, index: usize) -> &mut Session {
        let i = index.min(self.sessions.len() - 1);
        &mut self.sessions[i]
    }

    /// Switch by position. Out-of-range is ignored rather than clamped: a stray
    /// `Alt-7` with three sessions open should do nothing, not jump to the last.
    pub fn switch_to(&mut self, index: usize) -> bool {
        if index >= self.sessions.len() || index == self.current {
            return false;
        }
        self.current = index;
        self.sessions[index].last_used = std::time::Instant::now();
        true
    }

    pub fn cycle(&mut self, step: isize) {
        let n = self.sessions.len() as isize;
        let next = (self.current as isize + step).rem_euclid(n) as usize;
        self.switch_to(next);
    }

    /// Create a session and switch to it. Returns its index.
    pub fn create(&mut self, name: impl Into<String>, left: VfsPath, right: VfsPath) -> usize {
        let id = SessionId(self.next_id);
        self.next_id += 1;
        self.sessions.push(Session::new(id, name, left, right));
        self.current = self.sessions.len() - 1;
        self.current
    }

    /// Close a session. Refuses to close the last one — an application with no
    /// session has nothing to draw, and "quit" is a different action with a
    /// different confirmation.
    pub fn close(&mut self, index: usize) -> Result<(), CloseError> {
        if self.sessions.len() == 1 {
            return Err(CloseError::LastSession);
        }
        if index >= self.sessions.len() {
            return Err(CloseError::NoSuchSession);
        }
        self.sessions.remove(index);
        // Keep looking at the same session where possible; otherwise step back
        // so closing the last one in the list does not wrap to the first.
        if self.current > index || self.current >= self.sessions.len() {
            self.current = self.current.saturating_sub(1);
        }
        Ok(())
    }

    /// Rename a session. An empty or duplicate name is refused rather than
    /// accepted, because the rail identifies sessions by name.
    pub fn rename(&mut self, index: usize, name: &str) -> Result<(), RenameError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(RenameError::Empty);
        }
        if self
            .sessions
            .iter()
            .enumerate()
            .any(|(i, s)| i != index && s.name == name)
        {
            return Err(RenameError::Taken);
        }
        match self.sessions.get_mut(index) {
            Some(s) => {
                s.name = name.to_string();
                Ok(())
            }
            None => Err(RenameError::NoSuchSession),
        }
    }

    /// A name not already taken, for a session created without one.
    pub fn unused_name(&self) -> String {
        for n in 1..=999 {
            let candidate = format!("session {n}");
            if !self.sessions.iter().any(|s| s.name == candidate) {
                return candidate;
            }
        }
        format!("session {}", self.next_id)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RenameError {
    #[error("a session needs a name")]
    Empty,
    #[error("another session already has that name")]
    Taken,
    #[error("no such session")]
    NoSuchSession,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CloseError {
    #[error("cannot close the last session")]
    LastSession,
    #[error("no such session")]
    NoSuchSession,
}

/// `/Users/x/prj/dmac` -> `~/prj/dmac`. Space is short wherever a path is shown
/// beside something else, and the home prefix is the least informative part of
/// any path.
pub fn abbreviate_home(path: &str) -> String {
    let home = if cfg!(windows) {
        std::env::var("USERPROFILE").ok()
    } else {
        std::env::var("HOME").ok()
    };
    match home {
        Some(h) if !h.is_empty() && path.starts_with(&h) => format!("~{}", &path[h.len()..]),
        _ => path.to_string(),
    }
}

/// Where per-session scratch belonging to DMACommander lives.
///
/// Beside the session file, so a session and what serves it are removed
/// together and a backup of one carries the other.
pub fn agent_root() -> Option<std::path::PathBuf> {
    crate::store::SessionStore::platform_default()
        .ok()
        .and_then(|s| s.path().parent().map(std::path::Path::to_path_buf))
}

/// The file a directory tree uses to say which session it belongs to.
pub const MARKER: &str = ".dmac-session";

/// Enough of one to hold a name, and not enough to be worth reading if it is
/// not one. A file that opens with a megabyte of anything else is not going to
/// turn into a session name further in.
const MARKER_LIMIT: u64 = 4096;

/// Long enough for a name someone chose, short enough to leave the rail room
/// for the paths beside it.
const NAME_LIMIT: usize = 64;

/// What a tree had to say about which session it belongs to.
///
/// Three answers and not two, because "there is a file and it is not usable" is
/// not the same as "there is no file": the first is worth a line on the way
/// past, and silently starting `main` instead would leave someone looking at
/// the wrong workspace with no idea why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Marker {
    /// Nothing above this directory claims it.
    None,
    /// A name, trimmed and checked.
    Named(String),
    /// A file was found and what is in it cannot be a session name. Carries the
    /// path, because the message has to say which file to go and fix.
    Unusable(std::path::PathBuf),
}

/// The session a directory belongs to, from the nearest `.dmac-session` at or
/// above it.
///
/// The third step of the resolution order — after `--session` and
/// `$DMAC_SESSION`, before auto-resume and the picker — and the one that makes
/// a checkout carry its own workspace: `cd` into it from anywhere and the
/// panels, the shells and the agent conversation come back.
///
/// Nearest wins, the way every other per-directory file in a tree behaves: a
/// project inside a checkout that has its own opinion is the one that counts.
/// The first file found decides, even when it decides badly — walking past an
/// unusable one to a usable one further up would answer a question nobody
/// asked.
pub fn session_from_tree(start: &std::path::Path) -> Marker {
    use std::io::Read;
    for dir in start.ancestors() {
        let path = dir.join(MARKER);
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        let mut text = String::new();
        // Bounded, and `read_to_string` so a file that is not text is refused
        // here rather than reaching the rail as replacement characters.
        if file.take(MARKER_LIMIT).read_to_string(&mut text).is_err() {
            return Marker::Unusable(path);
        }
        let named = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .and_then(clean_name);
        return match named {
            Some(name) => Marker::Named(name),
            None => Marker::Unusable(path),
        };
    }
    Marker::None
}

/// A session name that came out of a file, or `None` if what came out cannot be
/// one.
///
/// Narrow on purpose. This is content, not configuration somebody typed: a
/// `.dmac-session` arrives with a `git clone` like any other file in the tree.
/// The name reaches the rail, the session file and every hosted shell as
/// `$DMAC_SESSION`, so a control character in it could rewrite the strip with
/// escape sequences, a separator would read as a path to whatever consumes the
/// environment, and a very long one pushes the paths off the side.
fn clean_name(line: &str) -> Option<String> {
    let name = line.trim();
    if name.is_empty() || name.chars().count() > NAME_LIMIT {
        return None;
    }
    if name == "." || name == ".." {
        return None;
    }
    let unusable = |c: char| c.is_control() || c == '/' || c == '\\';
    (!name.chars().any(unusable)).then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mgr() -> SessionManager {
        SessionManager::new("work", VfsPath::local("/a"), VfsPath::local("/b"))
    }

    /// A directory deep inside a tree belongs to the session the tree claims,
    /// which is the whole point: `cd` in from anywhere and the workspace comes
    /// back with you.
    #[test]
    fn a_marker_is_found_from_anywhere_below_it() {
        let tmp = tempfile::tempdir().unwrap();
        let deep = tmp.path().join("a/b/c");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(tmp.path().join(MARKER), "work\n").unwrap();
        assert_eq!(session_from_tree(&deep), Marker::Named("work".into()));
    }

    /// Nearest wins: a project inside a checkout is allowed its own opinion.
    #[test]
    fn the_nearest_marker_decides() {
        let tmp = tempfile::tempdir().unwrap();
        let inner = tmp.path().join("inner");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(tmp.path().join(MARKER), "outer").unwrap();
        std::fs::write(inner.join(MARKER), "inner").unwrap();
        assert_eq!(session_from_tree(&inner), Marker::Named("inner".into()));
    }

    /// A file people will edit by hand deserves comments and room to breathe.
    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(MARKER),
            "# the session this checkout belongs to\n\n  review  \nignored\n",
        )
        .unwrap();
        assert_eq!(
            session_from_tree(tmp.path()),
            Marker::Named("review".into())
        );
    }

    /// The file arrives with a `git clone` like everything else in the tree, so
    /// what it may contain is worth being narrow about: an escape sequence in a
    /// session name is an escape sequence in the rail.
    #[test]
    fn a_name_that_could_not_be_one_is_refused_and_named() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join(MARKER);
        for bad in [
            "\u{1b}[2J\u{1b}[H",
            "../../elsewhere",
            "one\\two",
            &"x".repeat(NAME_LIMIT + 1),
            "#only a comment",
            "",
        ] {
            std::fs::write(&marker, bad).unwrap();
            assert_eq!(
                session_from_tree(tmp.path()),
                Marker::Unusable(marker.clone()),
                "{bad:?} should not become a session name"
            );
        }
    }

    /// An unusable file stops the walk rather than being stepped over: the
    /// answer to "which session is this" must not quietly come from a directory
    /// further up than the one that tried to answer.
    #[test]
    fn an_unusable_marker_is_not_walked_past() {
        let tmp = tempfile::tempdir().unwrap();
        let inner = tmp.path().join("inner");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(tmp.path().join(MARKER), "outer").unwrap();
        std::fs::write(inner.join(MARKER), "  ").unwrap();
        assert_eq!(
            session_from_tree(&inner),
            Marker::Unusable(inner.join(MARKER))
        );
    }

    #[test]
    fn no_marker_anywhere_is_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(session_from_tree(tmp.path()), Marker::None);
    }

    #[test]
    fn there_is_always_exactly_one_session_to_start_with() {
        let m = mgr();
        assert_eq!(m.len(), 1);
        assert_eq!(m.current().name, "work");
    }

    /// The property that makes the rail worth having: switching does not reload.
    #[test]
    fn switching_preserves_each_sessions_state() {
        let mut m = mgr();
        m.current_mut().command_line.push_str("first");
        m.create("second", VfsPath::local("/c"), VfsPath::local("/d"));
        m.current_mut().command_line.push_str("second");

        m.switch_to(0);
        assert_eq!(m.current().command_line, "first");
        m.switch_to(1);
        assert_eq!(
            m.current().command_line,
            "second",
            "state must survive a switch"
        );
    }

    #[test]
    fn creating_a_session_switches_to_it() {
        let mut m = mgr();
        let i = m.create("other", VfsPath::local("/c"), VfsPath::local("/d"));
        assert_eq!(m.current_index(), i);
        assert_eq!(m.current().name, "other");
    }

    #[test]
    fn cycling_wraps_in_both_directions() {
        let mut m = mgr();
        m.create("b", VfsPath::local("/"), VfsPath::local("/"));
        m.create("c", VfsPath::local("/"), VfsPath::local("/"));
        m.switch_to(0);
        m.cycle(-1);
        assert_eq!(m.current().name, "c");
        m.cycle(1);
        assert_eq!(m.current().name, "work");
    }

    /// A stray Alt-7 with three sessions open must do nothing, not jump to the
    /// last one — silently acting on the wrong session is worse than ignoring it.
    #[test]
    fn switching_out_of_range_is_ignored_not_clamped() {
        let mut m = mgr();
        m.create("b", VfsPath::local("/"), VfsPath::local("/"));
        m.switch_to(0);
        assert!(!m.switch_to(9));
        assert_eq!(m.current_index(), 0);
    }

    #[test]
    fn the_last_session_cannot_be_closed() {
        let mut m = mgr();
        assert_eq!(m.close(0), Err(CloseError::LastSession));
        assert_eq!(
            m.len(),
            1,
            "the application must always have something to draw"
        );
    }

    #[test]
    fn closing_before_the_current_one_keeps_you_on_the_same_session() {
        let mut m = mgr();
        m.create("b", VfsPath::local("/"), VfsPath::local("/"));
        m.create("c", VfsPath::local("/"), VfsPath::local("/"));
        m.switch_to(2);
        m.close(0).expect("closable");
        assert_eq!(m.current().name, "c", "the view must not jump elsewhere");
    }

    #[test]
    fn closing_the_current_last_session_steps_back() {
        let mut m = mgr();
        m.create("b", VfsPath::local("/"), VfsPath::local("/"));
        m.switch_to(1);
        m.close(1).expect("closable");
        assert_eq!(m.current().name, "work");
        assert_eq!(m.current_index(), 0);
    }

    #[test]
    fn closing_a_session_that_does_not_exist_is_an_error_not_a_panic() {
        let mut m = mgr();
        m.create("b", VfsPath::local("/"), VfsPath::local("/"));
        assert_eq!(m.close(9), Err(CloseError::NoSuchSession));
    }

    /// Positions shift when a session closes, so anything held across time has
    /// to be an id.
    #[test]
    fn a_session_is_found_by_id_after_the_list_shifts() {
        let mut m = mgr();
        m.create("b", VfsPath::local("/"), VfsPath::local("/"));
        m.create("c", VfsPath::local("/"), VfsPath::local("/"));
        let id_c = m.all()[2].id;
        assert_eq!(m.index_of_id(id_c), Some(2));
        m.close(0).expect("closable");
        assert_eq!(
            m.index_of_id(id_c),
            Some(1),
            "the id must survive the shift"
        );
    }

    #[test]
    fn an_id_from_a_closed_session_resolves_to_nothing() {
        let mut m = mgr();
        m.create("b", VfsPath::local("/"), VfsPath::local("/"));
        let id = m.all()[1].id;
        m.close(1).expect("closable");
        assert_eq!(m.index_of_id(id), None);
    }

    #[test]
    fn renaming_rejects_empty_and_duplicate_names() {
        let mut m = mgr();
        m.create("other", VfsPath::local("/"), VfsPath::local("/"));
        assert_eq!(m.rename(1, "   "), Err(RenameError::Empty));
        assert_eq!(m.rename(1, "work"), Err(RenameError::Taken));
        assert_eq!(m.rename(1, "  build  "), Ok(()));
        assert_eq!(m.all()[1].name, "build", "the name is trimmed");
    }

    #[test]
    fn a_session_can_keep_its_own_name() {
        let mut m = mgr();
        assert_eq!(
            m.rename(0, "work"),
            Ok(()),
            "renaming to itself is not a clash"
        );
    }

    #[test]
    fn generated_names_do_not_collide() {
        let mut m = mgr();
        for _ in 0..5 {
            let name = m.unused_name();
            m.create(name, VfsPath::local("/"), VfsPath::local("/"));
        }
        let mut names: Vec<&str> = m.all().iter().map(|s| s.name.as_str()).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before, "duplicate session names");
    }

    #[test]
    fn adjacent_sessions_get_different_colours() {
        let mut m = mgr();
        m.create("b", VfsPath::local("/"), VfsPath::local("/"));
        assert_ne!(m.all()[0].color, m.all()[1].color);
    }

    #[test]
    fn the_home_prefix_is_abbreviated_in_the_rail() {
        // Only meaningful when HOME is set, which it is everywhere we run.
        if let Ok(home) = std::env::var("HOME") {
            let p = format!("{home}/prj/dmac");
            assert_eq!(abbreviate_home(&p), "~/prj/dmac");
        }
        assert_eq!(abbreviate_home("/etc/hosts"), "/etc/hosts");
    }
}
