//! Hosting child processes in a pseudo-terminal.
//!
//! Owned jointly by the `tui-engineer` and `session-engineer` agents.
//!
//! Why a PTY and not window embedding: see `docs/adr/0002`. Window reparenting
//! has no public API on macOS and is deliberately impossible on Wayland, while
//! a PTY works identically on all three platforms *and* over SSH. A hosted
//! process is genuinely ours — we own its stdin, its stdout, its size and its
//! lifetime.
// Tests assert; `unwrap`/`expect` there are how a failure is reported.
// In non-test code the workspace lints still forbid them.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Serialises [`Hosted::spawn`]'s call into the pty layer. See the comment at
/// the call site for why this exists at all.
fn open_lock() -> &'static Mutex<()> {
    static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    #[error("could not open a pseudo-terminal: {0}")]
    Open(String),
    #[error("could not start {program}: {source}")]
    Spawn {
        program: String,
        #[source]
        source: anyhow::Error,
    },
    #[error("the hosted process has exited")]
    Gone,
    #[error("io error talking to the hosted process: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, PtyError>;

/// Called from the reader thread when the hosted screen has changed.
///
/// A hosted program speaks whenever it likes, and nothing else in the
/// application is listening: without this the pane only repainted when the user
/// happened to press a key, so `ping`, a build, or anything with a delay sat
/// invisible until it was nudged. The waker is how the child gets to say
/// "there is something new to look at".
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// A rectangle of terminal cells produced by a hosted program, as understood by
/// a real terminal emulator.
///
/// Exposed as a `vt100::Screen` rather than as raw bytes because a hosted
/// program does not emit a picture — it emits a stream of cursor movements and
/// attribute changes that only mean something once interpreted.
pub type Screen<'a> = vt100::Screen;

/// Everything needed to start a hosted child.
///
/// A struct rather than eight positional arguments, which is how two `u16`s
/// next to each other end up swapped.
pub struct Spawn<'a> {
    pub program: &'a str,
    pub args: &'a [String],
    pub cwd: Option<&'a Path>,
    pub cols: u16,
    pub rows: u16,
    pub scrollback: usize,
    /// Extra environment for the child, applied over the defaults.
    pub env: &'a [(String, String)],
    /// Called whenever the child changes the screen. `None` gives a shell whose
    /// output is still parsed correctly but which nothing will repaint on its
    /// own — only tests want that.
    pub waker: Option<Waker>,
}

impl<'a> Spawn<'a> {
    /// A plain child: no extra environment, nothing listening for output.
    pub fn new(program: &'a str, args: &'a [String], cols: u16, rows: u16) -> Self {
        Self {
            program,
            args,
            cwd: None,
            cols,
            rows,
            scrollback: 100,
            env: &[],
            waker: None,
        }
    }
}

/// A child process running on a pseudo-terminal.
///
/// Output is drained by a dedicated thread into a `vt100` parser behind a mutex,
/// so a program that floods stdout can never stall the UI: the reader keeps
/// consuming and the renderer takes whatever the screen says whenever it draws.
pub struct Hosted {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    parser: Arc<Mutex<vt100::Parser>>,
    /// Set by the reader thread when the process closes its end.
    finished: Arc<AtomicBool>,
    /// Set by the reader thread when the screen has changed and cleared by the
    /// renderer once it has drawn it. Not a queue: an arbitrary amount of
    /// output collapses into one "needs repainting", which is what keeps a
    /// program flooding stdout from driving one frame per buffer.
    dirty: Arc<AtomicBool>,
    cols: u16,
    rows: u16,
    program: String,
    /// The hosted tree as it stood when teardown began.
    ///
    /// Captured once, before anything is signalled: the moment the shell dies
    /// its children are re-parented to init and nothing connects them to us any
    /// more, so a list built afterwards is empty exactly when it matters.
    #[cfg(unix)]
    doomed: Vec<libc::pid_t>,
}

impl Hosted {
    /// Spawn a child on a PTY, as described by `spec`.
    pub fn spawn(spec: Spawn<'_>) -> Result<Self> {
        let Spawn {
            program,
            args,
            cwd,
            cols,
            rows,
            scrollback,
            env,
            waker,
        } = spec;
        let (cols, rows) = (cols.max(2), rows.max(2));

        // One at a time. Opening a pseudo-terminal is three syscalls that have
        // to agree with each other — claim a master, change the slave's
        // ownership, unlock it — and on macOS two threads doing that at once
        // intermittently lose the race, with `openpty` failing outright rather
        // than retrying. It shows up as an occasional "Unknown error: -6" when
        // several sessions are restored together, and as a flaky test suite.
        //
        // The lock costs nothing: this happens once per shell, not once per
        // keystroke, and the critical section is microseconds long.
        let pty = {
            let _one_at_a_time = open_lock()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            native_pty_system()
                .openpty(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|e| PtyError::Open(e.to_string()))?
        };

        let mut cmd = CommandBuilder::new(program);
        for a in args {
            cmd.arg(a);
        }
        if let Some(dir) = cwd {
            cmd.cwd(dir);
        }
        // Tell the child what it is talking to. Without TERM most programs fall
        // back to a dumb terminal and stop using colour or cursor addressing.
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        // A marker so a shell's rc files, and the user, can tell where they are.
        cmd.env("DMAC", "1");
        // Whatever the caller wants the child to know about — the session's
        // agent id and the shim directory that uses it, in practice. Applied
        // last so a caller can override anything set above.
        for (k, v) in env {
            cmd.env(k, v);
        }

        let child = pty.slave.spawn_command(cmd).map_err(|e| PtyError::Spawn {
            program: program.to_string(),
            source: e,
        })?;

        // Drop the slave handle: while we hold it the PTY never reports EOF, so
        // the reader thread would hang forever after the child exits.
        drop(pty.slave);

        let writer = pty.master.take_writer().map_err(|e| PtyError::Spawn {
            program: program.to_string(),
            source: e,
        })?;
        let mut reader = pty.master.try_clone_reader().map_err(|e| PtyError::Spawn {
            program: program.to_string(),
            source: e,
        })?;

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, scrollback)));
        let finished = Arc::new(AtomicBool::new(false));
        let dirty = Arc::new(AtomicBool::new(false));

        {
            let parser = Arc::clone(&parser);
            let finished = Arc::clone(&finished);
            let dirty = Arc::clone(&dirty);
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            // The lock is held only for the parse, never across
                            // the read, so a flooding child cannot block a draw
                            // for longer than one buffer.
                            if let Ok(mut p) = parser.lock() {
                                p.process(&buf[..n]);
                            }
                            // Wake only on the clean-to-dirty edge. While a
                            // repaint is still owed, further output changes
                            // nothing that has not already been asked for, so
                            // `yes` costs the UI exactly as much as `echo`.
                            if !dirty.swap(true, Ordering::AcqRel)
                                && let Some(w) = &waker
                            {
                                w();
                            }
                        }
                    }
                }
                finished.store(true, Ordering::Relaxed);
                // Exiting is itself a visible change — the title gains
                // "(exited)" — so it needs a repaint like any other.
                dirty.store(true, Ordering::Release);
                if let Some(w) = &waker {
                    w();
                }
            });
        }

        Ok(Self {
            master: pty.master,
            writer,
            child,
            parser,
            finished,
            dirty,
            cols,
            rows,
            program: program.to_string(),
            #[cfg(unix)]
            doomed: Vec::new(),
        })
    }

    /// Spawn the user's login shell, interactively.
    pub fn shell(
        cwd: Option<&Path>,
        cols: u16,
        rows: u16,
        waker: Option<Waker>,
        env: &[(String, String)],
    ) -> Result<Self> {
        let shell = default_shell();
        // `-i` so rc files load and the prompt appears: a shell without them is
        // not the shell the user configured, and they notice immediately.
        let args = if cfg!(windows) {
            Vec::new()
        } else {
            vec!["-i".to_string()]
        };
        Self::spawn(Spawn {
            program: &shell,
            args: &args,
            cwd,
            cols,
            rows,
            scrollback: 2000,
            env,
            waker,
        })
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    /// Whether the child has closed its end of the PTY.
    pub fn finished(&self) -> bool {
        self.finished.load(Ordering::Relaxed)
    }

    /// The exit status, once there is one. `None` while still running.
    pub fn exit_status(&mut self) -> Option<u32> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(status.exit_code()),
            _ => None,
        }
    }

    pub fn size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    /// Whether the screen has changed since the last `mark_drawn`.
    pub fn dirty(&self) -> bool {
        self.dirty.load(Ordering::Acquire)
    }

    /// Say the current screen has been drawn, which re-arms the waker.
    ///
    /// Deliberately the renderer's job and not the reader's: leaving the flag
    /// set is how a caller throttles itself, because the reader stays silent
    /// for as long as a repaint is already owed.
    pub fn mark_drawn(&self) {
        self.dirty.store(false, Ordering::Release);
    }

    /// Tell the child the window changed size.
    ///
    /// Both the PTY and the emulator have to be told, or the two disagree about
    /// where the lines wrap and the display shears.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        let (cols, rows) = (cols.max(2), rows.max(2));
        if (cols, rows) == (self.cols, self.rows) {
            return Ok(());
        }
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| PtyError::Open(e.to_string()))?;
        if let Ok(mut p) = self.parser.lock() {
            p.screen_mut().set_size(rows, cols);
        }
        self.cols = cols;
        self.rows = rows;
        Ok(())
    }

    /// Send raw bytes to the child's stdin.
    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        if self.finished() {
            return Err(PtyError::Gone);
        }
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        Ok(())
    }

    /// Send a command line, as if typed and followed by Enter.
    pub fn run(&mut self, line: &str) -> Result<()> {
        self.write(line.as_bytes())?;
        self.write(b"\r")
    }

    /// Read the emulated screen.
    ///
    /// Takes a closure rather than returning a reference because the screen
    /// lives behind the mutex the reader thread also holds; handing out a
    /// borrow would mean handing out the lock.
    pub fn with_screen<R>(&self, f: impl FnOnce(&vt100::Screen) -> R) -> Option<R> {
        self.parser.lock().ok().map(|p| f(p.screen()))
    }

    /// How far the view has been pushed back from the live screen, in lines.
    /// `0` means the live screen is what you are looking at.
    pub fn scroll_offset(&self) -> usize {
        self.parser
            .lock()
            .ok()
            .map_or(0, |p| p.screen().scrollback())
    }

    /// How many lines are held above the live screen right now.
    ///
    /// `vt100` does not publish the length, so this asks for an impossible
    /// offset and reads back what it was clamped to — which *is* the length,
    /// by definition. Done under the lock and put straight back, so nothing
    /// ever sees the intermediate value.
    pub fn scrollback_len(&self) -> usize {
        self.parser.lock().ok().map_or(0, |mut p| {
            let saved = p.screen().scrollback();
            p.screen_mut().set_scrollback(usize::MAX);
            let len = p.screen().scrollback();
            p.screen_mut().set_scrollback(saved);
            len
        })
    }

    /// Move the view `delta` lines away from the live screen — positive goes
    /// back into history — and report how far it actually went.
    ///
    /// The *actual* distance, not the requested one, because a caller with a
    /// selection on screen has to move it by exactly as much as the text moved
    /// under it. Returning the request would drift the highlight off its text
    /// at the top and bottom of the buffer.
    pub fn scroll_by(&self, delta: i32) -> i32 {
        let Ok(mut p) = self.parser.lock() else {
            return 0;
        };
        let before = p.screen().scrollback();
        let want = (before as i64 + delta as i64).max(0) as usize;
        p.screen_mut().set_scrollback(want);
        let after = p.screen().scrollback();
        // Both fit in a `u16`-sized buffer many times over; the cast cannot
        // lose anything a 2000-line scrollback could produce.
        after as i32 - before as i32
    }

    /// Put the live screen back in view.
    pub fn scroll_to_bottom(&self) {
        if let Ok(mut p) = self.parser.lock() {
            p.screen_mut().set_scrollback(0);
        }
    }

    /// Text between two positions in the *buffer*: line 0 is the oldest line
    /// still held in the scrollback, and lines from `scrollback_len()` onwards
    /// are the live screen. `to.1` is exclusive, as a selection's far end is.
    ///
    /// Buffer coordinates rather than screen ones because a selection made by
    /// scrolling can be taller than the screen, and reading only the part that
    /// happens to be in view would hand back less than was selected without
    /// saying so. Rows outside the buffer are clamped into it rather than
    /// refused: the ends of a selection are allowed to be dragged past the ends
    /// of the text.
    pub fn contents_between_buffer(&self, from: (i64, u16), to: (i64, u16)) -> Option<String> {
        let mut p = self.parser.lock().ok()?;
        let saved = p.screen().scrollback();
        p.screen_mut().set_scrollback(usize::MAX);
        let held = p.screen().scrollback() as i64;
        let (rows, cols) = p.screen().size();
        let last = held + rows as i64 - 1;

        let first_row = from.0.clamp(0, last);
        let last_row = to.0.clamp(0, last);
        let mut out = String::new();
        let mut row = first_row;
        while row <= last_row {
            // The offset that brings `row` to the top of the view, or 0 once
            // `row` is in the live screen and cannot be brought any higher.
            let offset = (held - row).clamp(0, held);
            p.screen_mut().set_scrollback(offset as usize);
            // Where `row` and the last row of this chunk land in that view.
            let top = held - offset;
            let chunk_end = last_row.min(top + rows as i64 - 1);
            let y0 = (row - top) as u16;
            let y1 = (chunk_end - top) as u16;
            let start_col = if row == first_row { from.1 } else { 0 };
            let end_col = if chunk_end == last_row { to.1 } else { cols };
            out.push_str(&p.screen().contents_between(y0, start_col, y1, end_col));
            if chunk_end == last_row {
                break;
            }
            // `contents_between` puts no break after its last row, so the join
            // between two chunks has to supply the one the text needs.
            if !p.screen().row_wrapped(y1) {
                out.push('\n');
            }
            row = chunk_end + 1;
        }

        p.screen_mut().set_scrollback(saved);
        Some(out)
    }

    /// The full command lines of everything running inside this shell.
    ///
    /// Asked at shutdown, so the next run knows what to start again — and knows
    /// it exactly, arguments included. Empty on platforms where the tree cannot
    /// be walked, which reads as "nothing was running": the safe answer, since
    /// the cost is an agent not reattached rather than one started that the
    /// user never asked for.
    pub fn running_commands(&mut self) -> Vec<String> {
        #[cfg(unix)]
        {
            match self.child.process_id() {
                Some(pid) => descendants_with_command(pid as libc::pid_t)
                    .into_iter()
                    .map(|(_, c)| c)
                    .filter(|c| !c.is_empty())
                    .collect(),
                None => Vec::new(),
            }
        }
        #[cfg(not(unix))]
        {
            Vec::new()
        }
    }

    /// Where the child actually is, asked of the system rather than remembered.
    ///
    /// Remembering is not good enough: the commander only knows about the `cd`s
    /// it typed itself, and the whole point of a shell is that you type your
    /// own. A directory shown on the border has to be the one the shell is in,
    /// or it is worse than showing nothing — you would trust it.
    ///
    /// `None` when it cannot be had: a platform without an answer, a child that
    /// has gone, or a directory the kernel will not name.
    #[cfg(target_os = "macos")]
    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        let pid = self.child.process_id()? as libc::pid_t;
        // SAFETY: every field of it is an integer or a byte array, so all
        // zeroes is a valid value; the call overwrites it anyway.
        #[allow(unsafe_code)]
        let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
        // SAFETY: a pid, a constant, and a buffer whose size is its own. The
        // call fills the buffer or reports that it did not.
        #[allow(unsafe_code)]
        let got = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                std::ptr::from_mut(&mut info).cast(),
                size,
            )
        };
        if got != size {
            return None;
        }
        // `vip_path` is `MAXPATHLEN` bytes spelled as an array of arrays, so
        // it is flattened before being read as the C string it is.
        let bytes: Vec<u8> = info
            .pvi_cdir
            .vip_path
            .iter()
            .flatten()
            .take_while(|c| **c != 0)
            .map(|c| *c as u8)
            .collect();
        (!bytes.is_empty()).then(|| {
            use std::os::unix::ffi::OsStringExt;
            std::path::PathBuf::from(std::ffi::OsString::from_vec(bytes))
        })
    }

    #[cfg(target_os = "linux")]
    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        let pid = self.child.process_id()?;
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        None
    }

    /// Whether the child has nothing running in the foreground — that is,
    /// whether a shell is sitting at its prompt.
    ///
    /// The terminal's foreground process group is the shell itself exactly when
    /// no command is running in it. This is what makes it safe to type at a
    /// hosted shell on the user's behalf: without the check, a `cd` sent while
    /// the user has `vim` open is not a directory change, it is two stray
    /// keystrokes in the middle of their document.
    ///
    /// Answers `false` whenever it cannot tell, which is the safe direction:
    /// the cost is a directory that did not follow, not a corrupted file.
    pub fn at_prompt(&self) -> bool {
        #[cfg(unix)]
        {
            match (self.master.process_group_leader(), self.child.process_id()) {
                (Some(fg), Some(pid)) => fg >= 0 && fg as u32 == pid,
                _ => false,
            }
        }
        // Windows has no foreground process group to ask about, so there is no
        // way to know, and guessing is exactly what must not happen here.
        #[cfg(not(unix))]
        {
            false
        }
    }

    /// Send the child to `dir`, if it is idle enough for that to mean what it
    /// says. Returns whether anything was sent.
    pub fn cd(&mut self, dir: &Path) -> Result<bool> {
        if self.finished() || !self.at_prompt() {
            return Ok(false);
        }
        let line = if cfg!(windows) {
            format!(
                "cd /d {}",
                dmac_core::tools::shell_quote(&dir.to_string_lossy())
            )
        } else {
            format!(
                "cd -- {}",
                dmac_core::tools::shell_quote(&dir.to_string_lossy())
            )
        };
        self.run(&line)?;
        Ok(true)
    }

    /// Everything descended from the hosted child, plus the child.
    ///
    /// Signalling the process group is not enough on its own. An interactive
    /// shell has job control, so anything it starts gets a process group of its
    /// own — which is exactly what a hosted `claude` is. The group the shell is
    /// in does not contain it, and the foreground group only contains whichever
    /// job is in front right now.
    fn tree(&mut self) -> Vec<libc::pid_t> {
        let Some(root) = self.child.process_id() else {
            return Vec::new();
        };
        let mut out = vec![root as libc::pid_t];
        out.extend(descendants(root as libc::pid_t));
        out
    }

    /// Every process group the hosted tree could be in.
    ///
    /// A PTY child is a session leader, so its own pid is a group id and its
    /// descendants inherit that group. A program that deliberately makes its
    /// own group — anything that manages a terminal of its own — is caught by
    /// the second: the group currently in the foreground of this tty.
    #[cfg(unix)]
    fn groups(&mut self) -> Vec<libc::pid_t> {
        let mut out = Vec::with_capacity(2);
        if let Some(pid) = self.child.process_id() {
            out.push(pid as libc::pid_t);
        }
        if let Some(fg) = self.master.process_group_leader()
            && !out.contains(&fg)
        {
            out.push(fg);
        }
        out
    }

    /// Ask the whole hosted tree to go away, without waiting for it.
    ///
    /// `Child::kill` signals the direct child only — the shell. Anything the
    /// shell started outlives it, which is how a hosted `claude` survived
    /// DMACommander and went on holding the session it had opened, so the next
    /// run was told that session was already in use. Signalling the process
    /// *group* reaches the whole tree.
    ///
    /// SIGHUP rather than SIGKILL, because that is what a real terminal sends
    /// when its window closes, and what a well-behaved program listens for in
    /// order to save its state and let go of whatever it is holding. Something
    /// that ignores it gets SIGKILL from [`Hosted::terminate`] afterwards.
    // SAFETY: `killpg` takes two integers and returns one. It cannot touch this
    // process's memory. The group ids come from the child we spawned and from
    // the tty we own, so the only risk would be signalling a group that had
    // already exited and had its id reused — and that window is closed by the
    // child being reaped only after this, which keeps its id from coming back.
    #[allow(unsafe_code)]
    pub fn hangup(&mut self) {
        #[cfg(unix)]
        {
            // The list comes first, before a single signal is sent. A hangup
            // kills the shell immediately, and the instant it dies its children
            // are re-parented to init — so a list built even a moment later is
            // empty exactly when it matters, which is what made the first
            // version of this look like it worked.
            if self.doomed.is_empty() {
                self.doomed = self.tree();
            }
            let (groups, doomed) = (self.groups(), self.doomed.clone());
            // Descendants first: the groups miss background jobs entirely, and
            // signalling them before the shell dies is the only chance to
            // address them by name.
            for pid in doomed {
                // SIGCONT as well: a process stopped with Ctrl-Z cannot act on
                // a hangup until it is running again.
                unsafe {
                    libc::kill(pid, libc::SIGHUP);
                    libc::kill(pid, libc::SIGCONT);
                }
            }
            for g in groups {
                unsafe {
                    libc::killpg(g, libc::SIGHUP);
                    libc::killpg(g, libc::SIGCONT);
                }
            }
        }
    }

    /// Wait a little for the tree to go, then insist, and reap the child.
    ///
    /// Separate from [`Hosted::hangup`] so a program with several hosted shells
    /// can ask all of them to leave and then wait once, rather than paying the
    /// grace period again for every one of them.
    // SAFETY: as for `hangup` — two integers in, one out, and the child is not
    // reaped until afterwards, so its group id cannot have been reused.
    #[allow(unsafe_code)]
    pub fn terminate(&mut self, grace: Duration) {
        #[cfg(unix)]
        let groups = self.groups();
        #[cfg(unix)]
        if self.doomed.is_empty() {
            self.doomed = self.tree();
        }
        #[cfg(unix)]
        let tree = self.doomed.clone();

        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        #[cfg(unix)]
        {
            for g in groups {
                unsafe {
                    libc::killpg(g, libc::SIGKILL);
                }
            }
            // Anything that ignored the hangup, individually. A program that
            // traps SIGHUP and stays is precisely the one still holding the
            // thing the next run will ask for.
            for pid in tree {
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Ask the child, and everything it started, to go away.
    pub fn kill(&mut self) {
        self.hangup();
        self.terminate(Duration::from_millis(300));
    }
}

impl Drop for Hosted {
    fn drop(&mut self) {
        // A hosted process must not outlive the session that owns it, or the
        // user ends up with orphaned shells they cannot see or stop.
        if !self.finished() {
            self.kill();
        }
    }
}

impl std::fmt::Debug for Hosted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hosted")
            .field("program", &self.program)
            .field("size", &(self.cols, self.rows))
            .field("finished", &self.finished())
            .finish()
    }
}

/// Every process descended from `root`, children before parents.
///
/// Read from `ps` rather than from a crate: it is one fork at shutdown, it says
/// the same thing on macOS and Linux, and the alternative is either a large
/// dependency or platform-specific `sysctl` calls for a list this program looks
/// at once per session, on the way out.
#[cfg(unix)]
fn descendants(root: libc::pid_t) -> Vec<libc::pid_t> {
    descendants_with_command(root)
        .into_iter()
        .map(|(p, _)| p)
        .collect()
}

/// The same, with each process's command name.
///
/// Used to answer "was an agent running in this shell when we quit?", which is
/// what makes reattaching it on the way back in possible.
#[cfg(unix)]
fn descendants_with_command(root: libc::pid_t) -> Vec<(libc::pid_t, String)> {
    use std::collections::HashMap;

    let Ok(output) = std::process::Command::new("ps")
        .args(["-Ao", "pid=,ppid=,args="])
        .output()
    else {
        return Vec::new();
    };
    let mut children: HashMap<libc::pid_t, Vec<libc::pid_t>> = HashMap::new();
    let mut command: HashMap<libc::pid_t, String> = HashMap::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut it = line.split_whitespace();
        if let (Some(pid), Some(ppid)) = (it.next(), it.next())
            && let (Ok(pid), Ok(ppid)) = (pid.parse(), ppid.parse())
        {
            children.entry(ppid).or_default().push(pid);
            // The whole command line, arguments and all. The program name alone
            // is not enough to start something again as it was: a model, a
            // permission mode or a working directory chosen on the command line
            // is part of what the user set up, and dropping it silently gives
            // them back something that only looks like what they had.
            command.insert(pid, it.collect::<Vec<_>>().join(" "));
        }
    }

    // Breadth-first from the root, then reversed: killing children before their
    // parents keeps a supervisor from noticing and restarting one.
    let mut out: Vec<(libc::pid_t, String)> = Vec::new();
    let mut queue = vec![root];
    while let Some(pid) = queue.pop() {
        for &child in children.get(&pid).into_iter().flatten() {
            // A cycle is impossible in a process tree, but a pid that has been
            // reused between reading and walking is not; the guard costs
            // nothing and the alternative is an infinite loop at shutdown.
            if child != root && !out.iter().any(|(p, _)| *p == child) {
                out.push((child, command.get(&child).cloned().unwrap_or_default()));
                queue.push(child);
            }
        }
    }
    out.reverse();
    out
}

/// The user's shell, from the environment, with a platform-appropriate default.
pub fn default_shell() -> String {
    if cfg!(windows) {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wait for a predicate on the screen text, so tests do not race the child.
    fn wait_for(h: &Hosted, secs: f32, pred: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + Duration::from_secs_f32(secs);
        loop {
            let text = h.with_screen(|s| s.contents()).unwrap_or_default();
            if pred(&text) || Instant::now() > deadline {
                return text;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A child that has printed `n` numbered lines onto a 6-row screen, so
    /// most of them are in the scrollback and none of them is ambiguous.
    #[cfg(unix)]
    fn counted_to(n: u32) -> Hosted {
        let script = format!("i=1; while [ $i -le {n} ]; do echo line-$i; i=$((i+1)); done");
        let h = Hosted::spawn(Spawn {
            program: "/bin/sh",
            args: &["-c".into(), script],
            cols: 40,
            rows: 6,
            scrollback: 100,
            ..Spawn::new("", &[], 0, 0)
        })
        .expect("spawn");
        wait_for(&h, 5.0, |t| t.contains(&format!("line-{n}")));
        h
    }

    /// The point of a scrollback: what left the screen is still there to be
    /// read back. Without this the pane is a six-line window onto a program
    /// that has said far more than six lines.
    #[cfg(unix)]
    #[test]
    fn what_scrolled_off_the_top_is_still_held() {
        let mut h = counted_to(30);
        assert!(
            h.scrollback_len() >= 24,
            "only {} lines were kept",
            h.scrollback_len()
        );
        assert_eq!(
            h.scroll_offset(),
            0,
            "a fresh pane looks at the live screen"
        );

        // The first line is long gone from the screen.
        let visible = h.with_screen(|s| s.contents()).unwrap_or_default();
        assert!(!visible.contains("line-1\n"), "got: {visible:?}");

        // But not from the buffer.
        let all = h
            .contents_between_buffer((0, 0), (i64::MAX, 40))
            .unwrap_or_default();
        for i in [1u32, 7, 19, 30] {
            assert!(all.contains(&format!("line-{i}")), "line-{i} was lost");
        }
        h.kill();
    }

    /// Scrolling reports what the buffer actually gave, not what was asked for.
    /// A caller with a highlight on screen moves it by this number, and a
    /// request returned at the ends of the buffer would slide it off its text.
    #[cfg(unix)]
    #[test]
    fn scrolling_reports_the_distance_it_really_moved() {
        let mut h = counted_to(30);
        let held = h.scrollback_len() as i32;

        assert_eq!(h.scroll_by(5), 5);
        assert_eq!(h.scroll_offset(), 5);

        // Past the oldest line held, and no further.
        assert_eq!(h.scroll_by(i32::MAX), held - 5);
        assert_eq!(h.scroll_offset(), held as usize);
        assert_eq!(h.scroll_by(1), 0, "there is nothing older to show");

        // And all the way back, whatever is asked for.
        assert_eq!(h.scroll_by(i32::MIN), -held);
        assert_eq!(h.scroll_offset(), 0);
        assert_eq!(h.scroll_by(-1), 0, "the live screen is the bottom");

        h.scroll_by(4);
        h.scroll_to_bottom();
        assert_eq!(h.scroll_offset(), 0);
        h.kill();
    }

    /// A selection taller than the screen has to come back whole. Reading only
    /// the part that happened to be in view would hand back less than was
    /// selected while looking exactly right, which is the failure this whole
    /// coordinate system exists to prevent.
    #[cfg(unix)]
    #[test]
    fn text_can_be_read_across_the_scrollback_boundary() {
        let mut h = counted_to(30);
        let held = h.scrollback_len() as i64;

        // Ten lines ending inside the live screen, so the range spans the seam
        // between the scrollback and the screen itself.
        let text = h
            .contents_between_buffer((held - 5, 0), (held + 4, 40))
            .unwrap_or_default();
        let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
        assert!(
            lines.len() >= 8,
            "only {} lines came back: {text:?}",
            lines.len()
        );
        // Consecutive, in order, and no line repeated at the seam.
        let numbers: Vec<u32> = lines
            .iter()
            .filter_map(|l| l.trim().strip_prefix("line-"))
            .filter_map(|n| n.parse().ok())
            .collect();
        assert!(numbers.len() >= 8, "got {numbers:?} from {text:?}");
        for pair in numbers.windows(2) {
            assert_eq!(pair[1], pair[0] + 1, "out of order in {numbers:?}");
        }

        // Reading it does not move the view, whatever it had to do to get there.
        assert_eq!(h.scroll_offset(), 0);
        h.kill();
    }

    #[cfg(unix)]
    #[test]
    fn a_hosted_command_produces_output_on_the_screen() {
        let mut h = Hosted::spawn(Spawn {
            program: "/bin/sh",
            args: &["-c".into(), "echo hello-from-pty".into()],
            cols: 40,
            rows: 10,
            scrollback: 100,
            ..Spawn::new("", &[], 0, 0)
        })
        .expect("spawn");
        let text = wait_for(&h, 3.0, |t| t.contains("hello-from-pty"));
        assert!(text.contains("hello-from-pty"), "got: {text:?}");
        h.kill();
    }

    #[cfg(unix)]
    #[test]
    fn input_reaches_the_child_and_its_reply_comes_back() {
        let mut h = Hosted::spawn(Spawn {
            program: "/bin/sh",
            args: &[],
            cols: 60,
            rows: 12,
            scrollback: 100,
            ..Spawn::new("", &[], 0, 0)
        })
        .expect("spawn");
        h.run("echo round-trip-ok").expect("write");
        let text = wait_for(&h, 3.0, |t| t.contains("round-trip-ok"));
        assert!(text.contains("round-trip-ok"), "got: {text:?}");
        h.kill();
    }

    #[cfg(unix)]
    #[test]
    fn the_child_starts_in_the_directory_we_asked_for() {
        // Canonicalised, and compared on the last component: macOS resolves
        // TMPDIR to /private/var/folders/.../T, so matching the literal path we
        // passed in would fail for the wrong reason. A wide terminal keeps the
        // long path from wrapping mid-word.
        let dir = std::env::temp_dir();
        let real = dir.canonicalize().unwrap_or(dir.clone());
        let leaf = real
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "tmp".into());

        let mut h = Hosted::spawn(Spawn {
            program: "/bin/sh",
            args: &[],
            cwd: Some(&dir),
            cols: 200,
            rows: 10,
            scrollback: 100,
            ..Spawn::new("", &[], 0, 0)
        })
        .expect("spawn");
        h.run("pwd").expect("write");
        let text = wait_for(&h, 3.0, |t| t.contains(&leaf));
        assert!(text.contains(&leaf), "expected {leaf:?} in: {text:?}");
        h.kill();
    }

    /// A program that exits must be reported as gone, not left looking alive —
    /// otherwise the UI shows a dead shell you can type into forever.
    #[cfg(unix)]
    #[test]
    fn an_exiting_child_is_noticed() {
        let h = Hosted::spawn(Spawn {
            program: "/bin/sh",
            args: &["-c".into(), "true".into()],
            cols: 20,
            rows: 5,
            scrollback: 10,
            ..Spawn::new("", &[], 0, 0)
        })
        .expect("spawn");
        let deadline = Instant::now() + Duration::from_secs(3);
        while !h.finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(h.finished(), "the reader thread never saw EOF");
    }

    #[cfg(unix)]
    #[test]
    fn writing_to_a_dead_child_reports_it_rather_than_hanging() {
        let mut h = Hosted::spawn(Spawn {
            program: "/bin/sh",
            args: &["-c".into(), "true".into()],
            cols: 20,
            rows: 5,
            scrollback: 10,
            ..Spawn::new("", &[], 0, 0)
        })
        .expect("spawn");
        let deadline = Instant::now() + Duration::from_secs(3);
        while !h.finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(matches!(h.run("echo nope"), Err(PtyError::Gone)));
    }

    #[cfg(unix)]
    #[test]
    fn resizing_is_reflected_in_the_emulated_screen() {
        let mut h = Hosted::spawn(Spawn {
            program: "/bin/sh",
            args: &[],
            cols: 40,
            rows: 10,
            scrollback: 100,
            ..Spawn::new("", &[], 0, 0)
        })
        .expect("spawn");
        h.resize(100, 30).expect("resize");
        let (cols, rows) = h.size();
        assert_eq!((cols, rows), (100, 30));
        let dims = h.with_screen(|s| s.size()).expect("screen");
        assert_eq!(dims, (30, 100), "the emulator must be resized too");
        h.kill();
    }

    #[test]
    fn spawning_something_that_does_not_exist_is_an_error_not_a_panic() {
        let r = Hosted::spawn(Spawn {
            program: "definitely-not-a-real-program-xyz",
            args: &[],
            cols: 20,
            rows: 5,
            scrollback: 10,
            ..Spawn::new("", &[], 0, 0)
        });
        assert!(r.is_err());
    }

    #[test]
    fn a_degenerate_size_is_clamped_rather_than_rejected() {
        // A terminal really does report 0 columns mid-resize.
        let h = Hosted::spawn(Spawn {
            program: &default_shell(),
            args: &[],
            cols: 0,
            rows: 0,
            scrollback: 10,
            ..Spawn::new("", &[], 0, 0)
        });
        if let Ok(mut h) = h {
            assert_eq!(h.size(), (2, 2));
            h.kill();
        }
    }

    /// The bug this whole mechanism exists for: output that arrives while
    /// nobody is typing has to announce itself, or the pane silently freezes
    /// until the user presses a key.
    #[cfg(unix)]
    #[test]
    fn output_wakes_the_caller_without_any_input() {
        let woken = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&woken);
        let args = ["-c".to_string(), "sleep 0.2; echo late".to_string()];
        let h = Hosted::spawn(Spawn {
            waker: Some(Arc::new(move || flag.store(true, Ordering::Relaxed))),
            ..Spawn::new("/bin/sh", &args, 40, 10)
        })
        .expect("spawn");

        let deadline = Instant::now() + Duration::from_secs(3);
        while !woken.load(Ordering::Relaxed) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            woken.load(Ordering::Relaxed),
            "the child produced output and nothing was told about it"
        );
        assert!(h.dirty(), "the screen changed but was not marked dirty");
    }

    /// A program flooding stdout must not queue one wake-up per buffer: while a
    /// repaint is still owed there is nothing new to ask for.
    #[cfg(unix)]
    #[test]
    fn a_flood_of_output_does_not_produce_a_flood_of_wake_ups() {
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = Arc::clone(&count);
        let args = [
            "-c".to_string(),
            "i=0; while [ $i -lt 4000 ]; do echo flooding-the-terminal-with-output; i=$((i+1)); done".to_string(),
        ];
        let h = Hosted::spawn(Spawn {
            waker: Some(Arc::new(move || {
                c.fetch_add(1, Ordering::Relaxed);
            })),
            ..Spawn::new("/bin/sh", &args, 80, 24)
        })
        .expect("spawn");

        let deadline = Instant::now() + Duration::from_secs(10);
        while !h.finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        // One for the first buffer, one for the exit. Never one per buffer:
        // the child writes hundreds of them and we never drew in between.
        let n = count.load(Ordering::Relaxed);
        assert!(n <= 2, "expected the flood to coalesce, got {n} wake-ups");
    }

    /// After drawing, the next output has to wake us again — otherwise the
    /// throttle turns into a permanent freeze.
    #[cfg(unix)]
    #[test]
    fn marking_drawn_re_arms_the_waker() {
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = Arc::clone(&count);
        let mut h = Hosted::spawn(Spawn {
            waker: Some(Arc::new(move || {
                c.fetch_add(1, Ordering::Relaxed);
            })),
            ..Spawn::new("/bin/sh", &[], 60, 12)
        })
        .expect("spawn");

        wait_for(&h, 3.0, |_| h.dirty());
        h.mark_drawn();
        let before = count.load(Ordering::Relaxed);

        h.run("echo second-round").expect("write");
        let text = wait_for(&h, 3.0, |t| t.contains("second-round"));
        assert!(text.contains("second-round"), "got: {text:?}");
        assert!(
            count.load(Ordering::Relaxed) > before,
            "output after a repaint never woke anyone"
        );
        h.kill();
    }

    /// The child going away is a visible change too: the pane's title says so.
    #[cfg(unix)]
    #[test]
    fn an_exit_asks_for_a_repaint() {
        let h = Hosted::spawn(Spawn {
            program: "/bin/sh",
            args: &["-c".into(), "true".into()],
            cols: 20,
            rows: 5,
            scrollback: 10,
            ..Spawn::new("", &[], 0, 0)
        })
        .expect("spawn");
        let deadline = Instant::now() + Duration::from_secs(3);
        while !h.finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(h.dirty(), "an exited child left nothing to redraw");
    }

    /// The escaping has to survive a real shell, not just look right.
    #[cfg(unix)]
    #[test]
    fn a_hostile_directory_name_is_entered_not_executed() {
        let base = std::env::temp_dir().join(format!("dmac-cd-{}", std::process::id()));
        let evil = base.join("a'b; touch PWNED $(id) `id`");
        if std::fs::create_dir_all(&evil).is_err() {
            eprintln!("skipping: this filesystem will not take that name");
            return;
        }

        let mut h = Hosted::spawn(Spawn {
            program: "/bin/sh",
            args: &[],
            cwd: Some(&base),
            cols: 200,
            rows: 10,
            scrollback: 200,
            ..Spawn::new("", &[], 0, 0)
        })
        .expect("spawn");
        wait_for(&h, 3.0, |t| !t.trim().is_empty());
        assert!(
            h.cd(&evil).expect("cd"),
            "the shell was idle; cd should have gone"
        );
        h.run("pwd").expect("pwd");
        let text = wait_for(&h, 3.0, |t| t.contains("a'b;"));
        h.kill();

        assert!(
            text.contains("a'b;"),
            "never arrived in the directory: {text:?}"
        );
        assert!(
            !base.join("PWNED").exists(),
            "the directory name was executed instead of entered"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The whole point of the idle check: a shell running something must not be
    /// typed into, or the `cd` lands in whatever has the terminal.
    #[cfg(unix)]
    #[test]
    fn a_busy_shell_is_left_alone() {
        let mut h = Hosted::spawn(Spawn {
            program: "/bin/sh",
            args: &[],
            cols: 80,
            rows: 10,
            scrollback: 200,
            ..Spawn::new("", &[], 0, 0)
        })
        .expect("spawn");
        wait_for(&h, 3.0, |t| !t.trim().is_empty());
        assert!(h.at_prompt(), "a fresh shell should be at its prompt");

        h.run("cat > /dev/null").expect("run");
        let busy = Instant::now() + Duration::from_secs(3);
        while h.at_prompt() && Instant::now() < busy {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!h.at_prompt(), "a foreground command should read as busy");
        assert!(
            !h.cd(&std::env::temp_dir()).expect("cd"),
            "sent a cd into a running program's stdin"
        );

        h.write(&[0x04]).expect("eof"); // end `cat`
        let idle = Instant::now() + Duration::from_secs(3);
        while !h.at_prompt() && Instant::now() < idle {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(h.at_prompt(), "the shell never came back to its prompt");
        h.kill();
    }

    /// The bug this exists for, reproduced the way it actually happens.
    ///
    /// Killing the shell is not enough and neither is signalling its process
    /// group. An interactive shell has job control, so what it starts gets a
    /// group of its own — a hosted `claude` is exactly that — and a program
    /// that traps SIGHUP stays put when the terminal goes away. It then goes on
    /// holding the session it opened, and the next run is told that session is
    /// already in use.
    #[cfg(unix)]
    #[test]
    fn a_job_in_its_own_group_that_ignores_a_hangup_is_still_killed() {
        let dir = std::env::temp_dir().join(format!("dmac-job-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let pidfile = dir.join("job.pid");

        let mut h = Hosted::shell(Some(&dir), 80, 24, None, &[]).expect("an interactive shell");
        wait_for(&h, 5.0, |t| !t.trim().is_empty());

        // Backgrounded from an interactive shell, so job control puts it in its
        // own process group; and deaf to every polite signal.
        h.run(&format!(
            "sh -c \"trap '' HUP TERM INT; sleep 300\" & echo $! > {}",
            pidfile.to_string_lossy()
        ))
        .expect("run");

        let deadline = Instant::now() + Duration::from_secs(5);
        let job = loop {
            if let Ok(text) = std::fs::read_to_string(&pidfile)
                && let Ok(pid) = text.trim().parse::<i32>()
                && alive(pid)
            {
                break pid;
            }
            assert!(Instant::now() < deadline, "the job never started");
            std::thread::sleep(Duration::from_millis(20));
        };
        // It really is in a group of its own, or the test proves nothing.
        #[allow(unsafe_code)]
        // SAFETY: one integer in, one out.
        let job_group = unsafe { libc::getpgid(job) };
        let shell = h.child.process_id().expect("a pid") as i32;
        assert_ne!(job_group, shell, "the job shared the shell's group");

        h.kill();

        let deadline = Instant::now() + Duration::from_secs(5);
        while alive(job) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        // Cleaned up either way, so a failure here does not leave a stray
        // process behind for the next run of the suite to trip over.
        let survived = alive(job);
        if survived {
            let _ = std::process::Command::new("kill")
                .args(["-9", &job.to_string()])
                .status();
        }
        assert!(!survived, "pid {job} outlived DMACommander");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn killing_a_shell_takes_its_children_with_it() {
        let dir = std::env::temp_dir().join(format!("dmac-tree-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let pidfile = dir.join("grandchild.pid");

        let mut h = Hosted::spawn(Spawn {
            program: "/bin/sh",
            args: &[
                "-c".into(),
                format!("sleep 300 & echo $! > {}; wait", pidfile.to_string_lossy()),
            ],
            cols: 40,
            rows: 10,
            scrollback: 100,
            ..Spawn::new("", &[], 0, 0)
        })
        .expect("spawn");

        // Wait for the grandchild to exist and announce itself.
        let deadline = Instant::now() + Duration::from_secs(5);
        let grandchild = loop {
            if let Ok(text) = std::fs::read_to_string(&pidfile)
                && let Ok(pid) = text.trim().parse::<i32>()
            {
                break pid;
            }
            assert!(Instant::now() < deadline, "the grandchild never started");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(alive(grandchild), "the grandchild should be running");

        h.kill();

        let deadline = Instant::now() + Duration::from_secs(5);
        while alive(grandchild) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !alive(grandchild),
            "pid {grandchild} outlived the shell that started it"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Signal 0 asks the kernel whether a process exists without disturbing it.
    #[cfg(unix)]
    #[allow(unsafe_code)]
    fn alive(pid: i32) -> bool {
        // SAFETY: two integers in, one out; signal 0 delivers nothing.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    /// Dropping a hosted shell has to clean up as thoroughly as killing it, or
    /// closing a session leaks the tree instead of the whole application doing.
    #[cfg(unix)]
    #[test]
    fn dropping_a_shell_also_takes_its_children() {
        let dir = std::env::temp_dir().join(format!("dmac-drop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let pidfile = dir.join("grandchild.pid");

        let grandchild = {
            let _h = Hosted::spawn(Spawn {
                program: "/bin/sh",
                args: &[
                    "-c".into(),
                    format!("sleep 300 & echo $! > {}; wait", pidfile.to_string_lossy()),
                ],
                cols: 40,
                rows: 10,
                scrollback: 100,
                ..Spawn::new("", &[], 0, 0)
            })
            .expect("spawn");

            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Ok(text) = std::fs::read_to_string(&pidfile)
                    && let Ok(pid) = text.trim().parse::<i32>()
                {
                    break pid;
                }
                assert!(Instant::now() < deadline, "the grandchild never started");
                std::thread::sleep(Duration::from_millis(20));
            }
        };

        let deadline = Instant::now() + Duration::from_secs(5);
        while alive(grandchild) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!alive(grandchild), "pid {grandchild} survived the drop");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A program that ignores the polite signal still has to go, or quitting
    /// would hang on anything that traps SIGHUP.
    #[cfg(unix)]
    #[test]
    fn something_that_ignores_a_hangup_is_still_killed() {
        let mut h = Hosted::spawn(Spawn {
            program: "/bin/sh",
            args: &["-c".into(), "trap '' HUP TERM; sleep 300".into()],
            cols: 40,
            rows: 10,
            scrollback: 100,
            ..Spawn::new("", &[], 0, 0)
        })
        .expect("spawn");
        let pid = 0; // only the shell matters here
        let _ = pid;
        let started = Instant::now();
        h.kill();
        assert!(
            h.finished() || h.exit_status().is_some() || started.elapsed() < Duration::from_secs(3)
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "quitting waited {:?} on a process that ignores signals",
            started.elapsed()
        );
    }
}
