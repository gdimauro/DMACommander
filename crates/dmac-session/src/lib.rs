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
            self.shell = Some(Hosted::shell(dir.as_deref(), cols, rows, waker)?);
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
}

impl SessionManager {
    pub fn new(name: impl Into<String>, left: VfsPath, right: VfsPath) -> Self {
        let first = Session::new(SessionId(0), name, left, right);
        Self {
            sessions: vec![first],
            current: 0,
            next_id: 1,
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

/// `/Users/x/prj/dmac` -> `~/prj/dmac`. The rail is narrow and the home prefix
/// is the least informative part of any path in it.
fn abbreviate_home(path: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn mgr() -> SessionManager {
        SessionManager::new("work", VfsPath::local("/a"), VfsPath::local("/b"))
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
