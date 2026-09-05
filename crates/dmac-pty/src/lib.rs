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
    cols: u16,
    rows: u16,
    program: String,
}

impl Hosted {
    /// Spawn `program` on a PTY of the given size, in `cwd`.
    pub fn spawn(
        program: &str,
        args: &[String],
        cwd: Option<&Path>,
        cols: u16,
        rows: u16,
        scrollback: usize,
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

        {
            let parser = Arc::clone(&parser);
            let finished = Arc::clone(&finished);
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
                        }
                    }
                }
                finished.store(true, Ordering::Relaxed);
            });
        }

        Ok(Self {
            master: pty.master,
            writer,
            child,
            parser,
            finished,
            cols,
            rows,
            program: program.to_string(),
        })
    }

    /// Spawn the user's login shell, interactively.
    pub fn shell(cwd: Option<&Path>, cols: u16, rows: u16) -> Result<Self> {
        let shell = default_shell();
        // `-i` so rc files load and the prompt appears: a shell without them is
        // not the shell the user configured, and they notice immediately.
        let args = if cfg!(windows) {
            Vec::new()
        } else {
            vec!["-i".to_string()]
        };
        Self::spawn(&shell, &args, cwd, cols, rows, 2000)
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
        )
        .expect("spawn");
        let text = wait_for(&h, 3.0, |t| t.contains("hello-from-pty"));
        assert!(text.contains("hello-from-pty"), "got: {text:?}");
        h.kill();
    }

    #[cfg(unix)]
    #[test]
    fn input_reaches_the_child_and_its_reply_comes_back() {
        let mut h = Hosted::spawn("/bin/sh", &[], None, 60, 12, 100).expect("spawn");
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

        let mut h = Hosted::spawn("/bin/sh", &[], Some(&dir), 200, 10, 100).expect("spawn");
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
        let h = Hosted::spawn("/bin/sh", &["-c".into(), "true".into()], None, 20, 5, 10)
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
        let mut h = Hosted::spawn("/bin/sh", &["-c".into(), "true".into()], None, 20, 5, 10)
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
        let mut h = Hosted::spawn("/bin/sh", &[], None, 40, 10, 100).expect("spawn");
        h.resize(100, 30).expect("resize");
        let (cols, rows) = h.size();
        assert_eq!((cols, rows), (100, 30));
        let dims = h.with_screen(|s| s.size()).expect("screen");
        assert_eq!(dims, (30, 100), "the emulator must be resized too");
        h.kill();
    }

    #[test]
    fn spawning_something_that_does_not_exist_is_an_error_not_a_panic() {
        let r = Hosted::spawn("definitely-not-a-real-program-xyz", &[], None, 20, 5, 10);
        assert!(r.is_err());
    }

    #[test]
    fn a_degenerate_size_is_clamped_rather_than_rejected() {
        // A terminal really does report 0 columns mid-resize.
        let h = Hosted::spawn(&default_shell(), &[], None, 0, 0, 10);
        if let Ok(mut h) = h {
            assert_eq!(h.size(), (2, 2));
            h.kill();
        }
    }
}
