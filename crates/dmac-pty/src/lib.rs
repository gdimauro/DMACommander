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
}

impl Hosted {
    /// Spawn `program` on a PTY of the given size, in `cwd`.
    ///
    /// `waker` is called whenever the child changes the screen. Passing `None`
    /// gives a shell whose output is still parsed correctly but which nothing
    /// will repaint on its own — only tests want that.
    pub fn spawn(
        program: &str,
        args: &[String],
        cwd: Option<&Path>,
        cols: u16,
        rows: u16,
        scrollback: usize,
        waker: Option<Waker>,
    ) -> Result<Self> {
        let (cols, rows) = (cols.max(2), rows.max(2));

        let pty = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| PtyError::Open(e.to_string()))?;

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
        })
    }

    /// Spawn the user's login shell, interactively.
    pub fn shell(cwd: Option<&Path>, cols: u16, rows: u16, waker: Option<Waker>) -> Result<Self> {
        let shell = default_shell();
        // `-i` so rc files load and the prompt appears: a shell without them is
        // not the shell the user configured, and they notice immediately.
        let args = if cfg!(windows) {
            Vec::new()
        } else {
            vec!["-i".to_string()]
        };
        Self::spawn(&shell, &args, cwd, cols, rows, 2000, waker)
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

    /// Ask the child to terminate, then stop waiting for it.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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
    use std::time::{Duration, Instant};

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

    #[cfg(unix)]
    #[test]
    fn a_hosted_command_produces_output_on_the_screen() {
        let mut h = Hosted::spawn(
            "/bin/sh",
            &["-c".into(), "echo hello-from-pty".into()],
            None,
            40,
            10,
            100,
            None,
        )
        .expect("spawn");
        let text = wait_for(&h, 3.0, |t| t.contains("hello-from-pty"));
        assert!(text.contains("hello-from-pty"), "got: {text:?}");
        h.kill();
    }

    #[cfg(unix)]
    #[test]
    fn input_reaches_the_child_and_its_reply_comes_back() {
        let mut h = Hosted::spawn("/bin/sh", &[], None, 60, 12, 100, None).expect("spawn");
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

        let mut h = Hosted::spawn("/bin/sh", &[], Some(&dir), 200, 10, 100, None).expect("spawn");
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
        let h = Hosted::spawn(
            "/bin/sh",
            &["-c".into(), "true".into()],
            None,
            20,
            5,
            10,
            None,
        )
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
        let mut h = Hosted::spawn(
            "/bin/sh",
            &["-c".into(), "true".into()],
            None,
            20,
            5,
            10,
            None,
        )
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
        let mut h = Hosted::spawn("/bin/sh", &[], None, 40, 10, 100, None).expect("spawn");
        h.resize(100, 30).expect("resize");
        let (cols, rows) = h.size();
        assert_eq!((cols, rows), (100, 30));
        let dims = h.with_screen(|s| s.size()).expect("screen");
        assert_eq!(dims, (30, 100), "the emulator must be resized too");
        h.kill();
    }

    #[test]
    fn spawning_something_that_does_not_exist_is_an_error_not_a_panic() {
        let r = Hosted::spawn(
            "definitely-not-a-real-program-xyz",
            &[],
            None,
            20,
            5,
            10,
            None,
        );
        assert!(r.is_err());
    }

    #[test]
    fn a_degenerate_size_is_clamped_rather_than_rejected() {
        // A terminal really does report 0 columns mid-resize.
        let h = Hosted::spawn(&default_shell(), &[], None, 0, 0, 10, None);
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
        let h = Hosted::spawn(
            "/bin/sh",
            &["-c".into(), "sleep 0.2; echo late".into()],
            None,
            40,
            10,
            100,
            Some(Arc::new(move || flag.store(true, Ordering::Relaxed))),
        )
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
        let h = Hosted::spawn(
            "/bin/sh",
            &["-c".into(), "i=0; while [ $i -lt 4000 ]; do echo flooding-the-terminal-with-output; i=$((i+1)); done".into()],
            None,
            80,
            24,
            100,
            Some(Arc::new(move || {
                c.fetch_add(1, Ordering::Relaxed);
            })),
        )
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
        let mut h = Hosted::spawn(
            "/bin/sh",
            &[],
            None,
            60,
            12,
            100,
            Some(Arc::new(move || {
                c.fetch_add(1, Ordering::Relaxed);
            })),
        )
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
        let h = Hosted::spawn(
            "/bin/sh",
            &["-c".into(), "true".into()],
            None,
            20,
            5,
            10,
            None,
        )
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

        let mut h = Hosted::spawn("/bin/sh", &[], Some(&base), 200, 10, 200, None).expect("spawn");
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
        let mut h = Hosted::spawn("/bin/sh", &[], None, 80, 10, 200, None).expect("spawn");
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
}
