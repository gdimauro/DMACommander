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
    /// A move the panels made while the shell was busy, still owed to it.
    ///
    /// A `cd` cannot be typed at a running program — a line handed to `vim` is
    /// text in somebody's document, not a command — so the move is remembered
    /// rather than dropped, and written the moment the prompt comes back. That
    /// is as close as anything gets to the *program* following the panels:
    /// nothing can move a process that is already running, on any operating
    /// system. What can be moved is the shell it came from, in time for the
    /// next one.
    pub cwd_owed: bool,
    /// The conversation a hosted agent in this session belongs to.
    ///
    /// Generated once and then kept for the life of the session, because that
    /// is the whole point: come back tomorrow and `claude` rejoins the
    /// conversation you left, instead of starting a new one next to it.
    pub conversation: Option<String>,
    /// The editor window this session had open, and where it was.
    ///
    /// Four numbers and a directory, rather than a handle: the window belongs
    /// to another application and will not survive us, so what is kept is what
    /// it takes to make an equivalent one — which is the only sense in which a
    /// window can be "restored" across a restart at all.
    pub editor: Option<EditorWindow>,
    /// The conversation this session was forked from, when it was opened
    /// beside another one.
    ///
    /// A snapshot taken at birth, and persisted: the fork happens once, and it
    /// has to still be possible tomorrow if the commander was closed before the
    /// agent was ever started. Once the sibling's own conversation exists this
    /// is only history — coming back to it is an ordinary resume of its own id.
    pub parent_conversation: Option<String>,
    /// The session this one was opened beside, if it was.
    ///
    /// Two levels and no more. A session either stands on its own or hangs off
    /// one that does, and a sibling made from a nested session joins it rather
    /// than nesting under it — see [`SessionManager::create_sibling`]. A tree of
    /// arbitrary depth is a tree you have to navigate; two levels is a group,
    /// which is what a handful of agents working on one thing actually is.
    pub parent: Option<SessionId>,
    /// Whether this session's children are folded away in the rail and the
    /// menu. Meaningless on a session that has none.
    pub collapsed: bool,
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

/// A window's rectangle and the screen it was on, as a session keeps it.
///
/// Four numbers twice, in the coordinates the window server answers in —
/// top-left origin of the main screen. The screen is what makes the rectangle
/// portable: without it, `x = -3412` is a place only while the display to the
/// left is plugged in.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Placed {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// `(x, y, width, height)` of the screen. Absent for a placement recorded
    /// before screens were, which can only be put back by clamping.
    #[serde(default)]
    pub screen: Option<(i32, i32, u32, u32)>,
}

/// An editor window belonging to a session: which folder it had open, and
/// where it was — **for each arrangement of monitors it has been seen in**.
///
/// One place is not enough. The office is an ultrawide beside the laptop; home
/// is the laptop; the other office is two ordinary monitors. A window arranged
/// in one of those has a rectangle that means nothing in the others, and a
/// session that remembered only the last one would, every morning, put the
/// window somewhere the user then has to drag back. So each arrangement keeps
/// its own — keyed by [`dmac_desktop::arrangement_key`], a readable name for
/// the set of screens — and coming back to the office gets the office back.
///
/// The old shape, a single rectangle with no screen, is still read: it becomes
/// one entry under an empty key, and is used only when nothing better exists.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EditorWindow {
    pub dir: String,
    /// Arrangement key, to where the window was in that arrangement.
    #[serde(default)]
    pub placements: std::collections::BTreeMap<String, Placed>,
    // The four fields below are the shape written before arrangements were
    // remembered. Read so an old file loses nothing; never written again.
    #[serde(default, skip_serializing)]
    x: i32,
    #[serde(default, skip_serializing)]
    y: i32,
    #[serde(default, skip_serializing)]
    width: u32,
    #[serde(default, skip_serializing)]
    height: u32,
}

impl EditorWindow {
    pub fn new(dir: impl Into<String>) -> Self {
        Self {
            dir: dir.into(),
            placements: std::collections::BTreeMap::new(),
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        }
    }

    /// Fold a file written before arrangements existed into the new shape.
    ///
    /// Called after loading. The single rectangle such a file carried becomes
    /// the entry under the empty key — a placement with no arrangement, which
    /// [`placement_for`](Self::placement_for) falls back to last.
    pub fn migrate(&mut self) {
        if self.width > 0 && self.height > 0 && self.placements.is_empty() {
            self.placements.insert(
                String::new(),
                Placed {
                    x: self.x,
                    y: self.y,
                    width: self.width,
                    height: self.height,
                    screen: None,
                },
            );
        }
        self.x = 0;
        self.y = 0;
        self.width = 0;
        self.height = 0;
    }

    /// Remember where the window is in this arrangement.
    pub fn record(&mut self, key: &str, placed: Placed) {
        self.placements.insert(key.to_string(), placed);
    }

    /// Where the window was in this arrangement — or, failing that, the most
    /// recently recorded place anywhere, to be fitted onto the screens that
    /// exist. Exact wins over fitted, always: the whole point of keeping one
    /// per arrangement is that "where I left it" is a fact and not a guess.
    ///
    /// Returns whether the answer is exact, so the caller can put an exact one
    /// back as it is and only fit the other kind.
    pub fn placement_for(&self, key: &str) -> Option<(&Placed, bool)> {
        if let Some(p) = self.placements.get(key) {
            return Some((p, true));
        }
        // Any other arrangement's, to be fitted. The last recorded is the most
        // likely to resemble what the user wants; a BTreeMap has no notion of
        // that, so the choice is: a real arrangement over the migrated
        // no-arrangement one.
        self.placements
            .iter()
            .filter(|(k, _)| !k.is_empty())
            .chain(self.placements.iter().filter(|(k, _)| k.is_empty()))
            .map(|(_, p)| (p, false))
            .next()
    }
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
            cwd_owed: false,
            conversation: None,
            editor: None,
            parent_conversation: None,
            parent: None,
            collapsed: false,
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
        let parent = self.parent_conversation.clone();
        agent::environment(&id, &name, &conversation, parent.as_deref())
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
    ///
    /// A shell that was busy leaves the move [owed](Self::cwd_owed) rather than
    /// losing it: [`catch_up_cwd`](Self::catch_up_cwd) writes it as soon as the
    /// prompt is back.
    pub fn follow_panel_cwd(&mut self) {
        let cwd = self.cwd[Self::index_of(self.active)].clone();
        if !cwd.is_local() || self.shell_cwd.as_ref() == Some(&cwd) {
            // Nothing a shell could be sent to, or it is already there. Either
            // way nothing is outstanding — an owed `cd` from before is not owed
            // to a directory nobody is in any more.
            self.cwd_owed = false;
            return;
        }
        let Some(shell) = self.shell.as_mut() else {
            self.cwd_owed = false;
            return;
        };
        if matches!(shell.cd(cwd.as_path()), Ok(true)) {
            self.shell_cwd = Some(cwd);
            self.cwd_owed = false;
        } else {
            // Something is running in there. Remembered, not dropped: the panel
            // moved for a reason, and the reason is usually the next command.
            self.cwd_owed = true;
        }
    }

    /// Write a `cd` the shell was too busy to take, if it is idle now.
    ///
    /// Called on every frame, and free unless something is actually owed: the
    /// check behind it is one `tcgetpgrp` on the shell's own tty. There is no
    /// timer — a program exiting makes its shell print a prompt, and the bytes
    /// of that prompt are themselves the wake-up.
    pub fn catch_up_cwd(&mut self) {
        if !self.cwd_owed {
            return;
        }
        if self.shell.as_ref().is_none_or(|s| s.finished()) {
            self.cwd_owed = false;
            return;
        }
        self.follow_panel_cwd();
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
    /// Where the terminal this program runs in was, per arrangement of
    /// monitors. Global rather than per session: there is one terminal window,
    /// whichever session is showing in it.
    pub terminal: std::collections::BTreeMap<String, Placed>,
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
            terminal: std::collections::BTreeMap::new(),
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

    /// Each live shell's foreground process group, by session.
    ///
    /// One syscall per shell and no forks. What it is worth is not the number
    /// but the *change* in it: it moves exactly when a program starts or ends
    /// in that shell, which is the only moment the answer to "is an agent
    /// running here" can have changed.
    pub fn foreground_groups(&self) -> Vec<(SessionId, i32)> {
        self.sessions
            .iter()
            .filter_map(|s| Some((s.id, s.hosted()?.foreground_group()?)))
            .collect()
    }

    /// Each live shell's pid, by session.
    pub fn shell_pids(&self) -> Vec<(SessionId, u32)> {
        self.sessions
            .iter()
            .filter_map(|s| Some((s.id, s.hosted()?.pid()?)))
            .collect()
    }

    /// Record what each session is hosting, reading the process table once.
    ///
    /// Returns whether anything changed, so the caller knows whether this is
    /// worth a write. Two things are recorded, not one: the agent's command
    /// line, and — when that line names a conversation — the conversation
    /// itself, because the agent is the authority on which one it is in. A user
    /// who types `claude --resume <other>` in a hosted shell has moved the
    /// session to that conversation, and the session follows.
    #[cfg(unix)]
    pub fn observe_agents(&mut self, seen: &[(SessionId, Option<String>)]) -> bool {
        let mut changed = false;
        for (id, line) in seen {
            let Some(s) = self.sessions.iter_mut().find(|s| s.id == *id) else {
                continue;
            };
            if let Some(line) = line
                && let Some(c) = agent::conversation_of(line)
                && s.conversation.as_deref() != Some(c.as_str())
            {
                s.conversation = Some(c);
                changed = true;
            }
            if s.agent.as_deref() != line.as_deref() {
                s.agent = line.clone();
                changed = true;
            }
        }
        changed
    }

    /// Give every session a conversation id, and say whether that changed
    /// anything.
    ///
    /// The id is the whole of what makes a hosted agent resumable, and it used
    /// to be minted the first time something asked for it — inside
    /// [`Session::shell`], on a path that marks nothing as needing saving. A
    /// commander killed before the next unrelated write therefore had
    /// `conversation: null` on disk, and the next run minted a *different* id.
    /// The conversation the user spent the afternoon in was still sitting in
    /// the agent's own store, and nothing here could name it any more.
    ///
    /// Minting up front costs one UUID per session and closes that window
    /// completely: an id that exists before a shell does is an id the ordinary
    /// save path has already written.
    pub fn ensure_conversations(&mut self) -> bool {
        let mut minted = false;
        for s in &mut self.sessions {
            if s.conversation.is_none() {
                s.conversation = Some(dmac_core::tools::uuid_v4());
                minted = true;
            }
        }
        minted
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

    /// Open a session beside `anchor`, in its group.
    ///
    /// The parent is `anchor` when `anchor` stands on its own, and *`anchor`'s*
    /// parent when it does not. That is the whole of the two-level rule: a
    /// sibling made from a nested session joins it at its level instead of
    /// nesting under it, so a group can grow as wide as the work needs without
    /// ever growing deeper than a glance can follow.
    ///
    /// Placed at the end of its parent's group, so the list stays in the order
    /// the tree is drawn in. Everything here indexes by position — the rail,
    /// the menu, the click that picks a row — and a subtree that is contiguous
    /// is one that needs no second ordering to draw.
    pub fn create_sibling(
        &mut self,
        anchor: usize,
        name: impl Into<String>,
        left: VfsPath,
        right: VfsPath,
    ) -> usize {
        let Some(a) = self.sessions.get(anchor) else {
            return self.create(name, left, right);
        };
        let parent = a.parent.unwrap_or(a.id);
        let Some(pos) = self.sessions.iter().position(|s| s.id == parent) else {
            return self.create(name, left, right);
        };
        // Past the parent and everything already hanging off it.
        let mut at = pos + 1;
        while self
            .sessions
            .get(at)
            .is_some_and(|s| s.parent == Some(parent))
        {
            at += 1;
        }
        let id = SessionId(self.next_id);
        self.next_id += 1;
        let mut session = Session::new(id, name, left, right);
        session.parent = Some(parent);
        // The mother's conversation, as it is now. This is what lets the agent
        // started here begin where hers is rather than from nothing.
        session.parent_conversation = self.sessions.get(pos).and_then(|p| p.conversation.clone());
        self.sessions.insert(at, session);
        // A group you have just added to is a group you want to see.
        if let Some(p) = self.sessions.get_mut(pos) {
            p.collapsed = false;
        }
        self.current = at;
        at
    }

    /// How deep a session sits: 0 on its own, 1 inside a group.
    pub fn depth(&self, index: usize) -> usize {
        usize::from(self.sessions.get(index).is_some_and(|s| s.parent.is_some()))
    }

    /// Whether this session has any hanging off it.
    pub fn has_children(&self, index: usize) -> bool {
        let Some(id) = self.sessions.get(index).map(|s| s.id) else {
            return false;
        };
        self.sessions.iter().any(|s| s.parent == Some(id))
    }

    /// Fold a group away, or open it. Answers whether anything happened —
    /// a session with nothing under it has nothing to fold.
    pub fn toggle_collapsed(&mut self, index: usize) -> bool {
        if !self.has_children(index) {
            return false;
        }
        let folding = match self.sessions.get_mut(index) {
            Some(s) => {
                s.collapsed = !s.collapsed;
                s.collapsed
            }
            None => return false,
        };
        // Folding a group you are inside would hide the session you are looking
        // at, and there is nothing to draw in its place. Come out to the parent
        // first, which is where the fold happened.
        if folding && self.depth(self.current) == 1 && self.parent_of(self.current) == Some(index) {
            self.switch_to(index);
        }
        true
    }

    /// Where a session's parent sits, by position.
    pub fn parent_of(&self, index: usize) -> Option<usize> {
        let parent = self.sessions.get(index)?.parent?;
        self.sessions.iter().position(|s| s.id == parent)
    }

    /// The positions to draw, in order, with folded groups left out.
    ///
    /// One list, used by the rail and by the menu, because two of them would
    /// eventually disagree about what a click at row *n* selects.
    pub fn visible(&self) -> Vec<usize> {
        let folded: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|s| s.collapsed)
            .map(|s| s.id)
            .collect();
        (0..self.sessions.len())
            .filter(|&i| match self.sessions[i].parent {
                Some(p) => !folded.contains(&p),
                None => true,
            })
            .collect()
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
        // A group goes together. Closing the session a group hangs off and
        // leaving its children behind would orphan them into the top level,
        // where they are no longer the thing the user grouped — and refusing
        // instead would make a group something you cannot get rid of. How many
        // that is can be asked first, with [`group_size`](Self::group_size), so
        // the confirmation can say it.
        let going = self.group_size(index);
        if going >= self.sessions.len() {
            return Err(CloseError::LastSession);
        }
        self.sessions.drain(index..index + going);
        // Keep looking at the same session where possible; otherwise step back
        // so closing the last one in the list does not wrap to the first.
        if self.current > index || self.current >= self.sessions.len() {
            self.current = self
                .current
                .saturating_sub(going)
                .min(self.sessions.len() - 1);
        }
        Ok(())
    }

    /// How many sessions closing this one takes with it: itself, plus anything
    /// hanging off it.
    ///
    /// Asked before closing rather than reported after, because it is what the
    /// confirmation has to say. "Close 4 sessions?" is a question; "closed 4
    /// sessions" is a thing that already happened to you.
    pub fn group_size(&self, index: usize) -> usize {
        let Some(s) = self.sessions.get(index) else {
            return 0;
        };
        if s.parent.is_some() {
            return 1;
        }
        let id = s.id;
        1 + self
            .sessions
            .iter()
            .skip(index + 1)
            .take_while(|c| c.parent == Some(id))
            .count()
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

    /// A `cd` cannot be typed at a running program, so the move is owed and
    /// paid the moment the prompt comes back — which is the whole of what
    /// "the shell follows the panels" can mean while something is running in
    /// it. Nothing can move a process that has already started.
    #[cfg(unix)]
    #[test]
    fn a_move_made_while_the_shell_was_busy_is_paid_at_the_next_prompt() {
        use dmac_pty::{Hosted, Spawn};
        use std::time::{Duration, Instant};

        fn settle(s: &Session, want: bool) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if s.shell.as_ref().is_some_and(|h| h.at_prompt()) == want {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        let here = std::env::temp_dir().canonicalize().expect("temp dir");
        let mut s = Session::new(SessionId(1), "t", VfsPath::local("/"), VfsPath::local("/"));
        // A shell of our own rather than the user's: `/bin/sh` starts the same
        // way on every machine, and this test is about what happens after.
        s.shell = Some(
            Hosted::spawn(Spawn {
                program: "/bin/sh",
                args: &[],
                cols: 80,
                rows: 10,
                ..Spawn::new("", &[], 0, 0)
            })
            .expect("spawn"),
        );
        s.shell_cwd = Some(VfsPath::local("/"));
        settle(&s, true);

        // Something running in the foreground: the shell is no longer listening
        // for commands, it is feeding whatever is.
        s.shell.as_mut().expect("shell").run("cat > /dev/null").ok();
        settle(&s, false);

        s.cwd[0] = VfsPath::local(here.clone());
        s.follow_panel_cwd();
        assert!(s.cwd_owed, "the move was dropped instead of remembered");
        assert_eq!(
            s.shell_cwd,
            Some(VfsPath::local("/")),
            "a cd was typed into a running program"
        );
        s.catch_up_cwd();
        assert!(s.cwd_owed, "still busy, still owed");

        // The program exits; the prompt comes back.
        s.shell.as_mut().expect("shell").write(&[0x04]).ok();
        settle(&s, true);

        s.catch_up_cwd();
        assert!(!s.cwd_owed, "the prompt came back and nothing was paid");
        assert_eq!(
            s.shell_cwd.as_ref().map(|p| p.display()),
            Some(here.to_string_lossy().to_string()),
            "the shell was not sent after the panels"
        );
        s.shell.as_mut().expect("shell").kill();
    }

    /// Nothing to pay it to, nothing owed: an owed move must not outlive the
    /// shell it was owed to, or it lands in the next one.
    #[test]
    fn an_owed_move_dies_with_the_shell_it_was_owed_to() {
        let mut s = Session::new(SessionId(1), "t", VfsPath::local("/"), VfsPath::local("/"));
        s.cwd_owed = true;
        s.catch_up_cwd();
        assert!(!s.cwd_owed);
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

    /// Two levels, and the rule that keeps it at two: a sibling made from a
    /// nested session joins it rather than nesting under it. Otherwise a group
    /// of agents working on one thing turns into a tree you have to navigate.
    #[test]
    fn a_sibling_of_a_nested_session_joins_it_instead_of_nesting_under_it() {
        let mut m = SessionManager::new("work", VfsPath::local("/a"), VfsPath::local("/b"));
        let first = m.create_sibling(0, "one", VfsPath::local("/a"), VfsPath::local("/b"));
        assert_eq!(
            first, 1,
            "a group is contiguous, so a child sits after its parent"
        );
        assert_eq!(m.depth(first), 1);

        // Made *from the child*, and still at the child's level.
        let second = m.create_sibling(first, "two", VfsPath::local("/a"), VfsPath::local("/b"));
        assert_eq!(m.depth(second), 1, "the tree grew a third level");
        assert_eq!(m.parent_of(second), Some(0), "it joined the wrong group");
        assert_eq!(second, 2, "siblings go at the end of their group");

        // And a third, from the parent this time: same group, same level.
        let third = m.create_sibling(0, "three", VfsPath::local("/a"), VfsPath::local("/b"));
        assert_eq!(third, 3);
        assert_eq!(m.parent_of(third), Some(0));
        assert_eq!(m.group_size(0), 4);
    }

    /// A folded group is not drawn, and folding one you are inside would hide
    /// the session on screen with nothing to put in its place.
    #[test]
    fn folding_a_group_takes_you_out_of_it_first() {
        let mut m = SessionManager::new("work", VfsPath::local("/a"), VfsPath::local("/b"));
        let child = m.create_sibling(0, "one", VfsPath::local("/a"), VfsPath::local("/b"));
        m.switch_to(child);

        assert!(m.toggle_collapsed(0));
        assert_eq!(
            m.visible(),
            vec![0],
            "a folded group still draws its own row"
        );
        assert_eq!(
            m.current_index(),
            0,
            "the session on screen was folded away"
        );

        assert!(m.toggle_collapsed(0));
        assert_eq!(m.visible(), vec![0, 1]);
        // Nothing hangs off a child, so there is nothing there to fold.
        assert!(!m.toggle_collapsed(1));
    }

    /// A group goes together. Leaving the children behind would orphan them
    /// into the top level, where they are no longer what the user grouped.
    #[test]
    fn closing_a_group_closes_what_hangs_off_it() {
        let mut m = SessionManager::new("work", VfsPath::local("/a"), VfsPath::local("/b"));
        m.create("other", VfsPath::local("/c"), VfsPath::local("/d"));
        m.create_sibling(0, "one", VfsPath::local("/a"), VfsPath::local("/b"));
        m.create_sibling(0, "two", VfsPath::local("/a"), VfsPath::local("/b"));
        assert_eq!(m.len(), 4);
        assert_eq!(m.group_size(0), 3, "itself and the two hanging off it");
        assert_eq!(m.group_size(1), 1, "a child takes nothing with it");

        m.switch_to(0);
        m.close(0).expect("close");
        assert_eq!(m.len(), 1);
        assert_eq!(m.all()[0].name, "other");
        assert_eq!(
            m.current_index(),
            0,
            "the cursor has to land somewhere real"
        );

        // And the last group standing may not be closed, however many it holds.
        let mut m = SessionManager::new("work", VfsPath::local("/a"), VfsPath::local("/b"));
        m.create_sibling(0, "one", VfsPath::local("/a"), VfsPath::local("/b"));
        assert!(m.close(0).is_err(), "that would leave nothing to draw");
    }
}
