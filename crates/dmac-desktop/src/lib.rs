//! Launching, finding and raising external GUI windows, bound to sessions.
//!
//! Owned by the `tui-engineer` agent. See `docs/PLAN.md` milestone M2.
//!
//! This is the answer to "run many VS Code windows and switch between them from
//! the session list": we never embed another application's window, we ask the
//! window server to bring it forward. That is supported on macOS (behind the
//! Accessibility permission), on Windows, and on X11 — unlike embedding, which
//! is available on neither macOS nor Wayland.
//!
//! Nothing here is on the render path. Every call shells out and waits, so the
//! caller runs it off the UI thread and reports the answer when it arrives.
// Tests assert; `unwrap`/`expect` there are how a failure is reported.
// In non-test code the workspace lints still forbid them.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What went wrong, in terms the status bar can show a user.
#[derive(Debug)]
pub enum DesktopError {
    /// No editor to launch. Names what was looked for, so the fix is obvious.
    NoEditor(String),
    /// The editor was found but would not start.
    Launch(std::io::Error),
    /// Window placement is not implemented for this platform.
    Unsupported,
    /// The window server refused, most often for want of a permission.
    Placement(String),
}

impl fmt::Display for DesktopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoEditor(name) => write!(f, "no `{name}` on PATH — set DMAC_EDITOR"),
            Self::Launch(e) => write!(f, "could not start the editor: {e}"),
            Self::Unsupported => write!(f, "window placement is macOS-only for now"),
            Self::Placement(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for DesktopError {}

/// The editor to open directories in.
///
/// `DMAC_EDITOR` first, so anyone who does not use VS Code is not arguing with
/// a hardcoded name. `code` otherwise, because that is the one whose CLI can
/// reuse an existing window, which is the whole behaviour being asked for.
pub fn editor() -> Result<PathBuf, DesktopError> {
    resolve_editor(&std::env::var("DMAC_EDITOR").unwrap_or_else(|_| "code".into()))
}

/// The same, given the name instead of reading it. Split out so it can be
/// tested without writing to the environment — which is process-wide, and a
/// test that does it races every other test in the binary.
fn resolve_editor(name: &str) -> Result<PathBuf, DesktopError> {
    // A path is taken as given: someone who wrote one meant it.
    if name.contains('/') {
        let p = PathBuf::from(name);
        return if p.is_file() {
            Ok(p)
        } else {
            Err(DesktopError::NoEditor(name.into()))
        };
    }
    which(name).ok_or_else(|| DesktopError::NoEditor(name.into()))
}

/// What the editor is launched with: the directory, and deliberately nothing
/// else. Split out so the "nothing else" is a thing a test can hold on to.
fn editor_args(dir: &Path) -> Vec<std::ffi::OsString> {
    vec![dir.as_os_str().to_owned()]
}

fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|c| is_executable(c))
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// Open `dir` in the editor, without taking anything away.
///
/// The directory and nothing else. This used to pass `-r`, which is
/// `--reuse-window`: "force to open a folder in an already opened window". It
/// was chosen to avoid handing someone a fifth window, and it was the wrong
/// trade — reusing a window means the folder that was in it is gone, which
/// from the other side of the screen is indistinguishable from the editor
/// having closed. Losing what you had open is never worth saving a window.
///
/// Left to itself the editor does the right thing and does it better than we
/// could: a window already showing this folder comes forward, and otherwise it
/// opens one, in whichever way that user configured it to. It knows what it has
/// open, which is more than any window title we could match on would tell us.
pub fn open_editor(dir: &Path) -> Result<(), DesktopError> {
    let editor = editor()?;
    Command::new(editor)
        .args(editor_args(dir))
        // Detached from our terminal in all three directions. An editor that
        // wrote a line of its own to stdout would land in the middle of a
        // rendered frame.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(DesktopError::Launch)
}

/// How many windows the editor has right now.
///
/// Asked *before* launching, because "the editor's window" is not a thing that
/// can be identified afterwards: a second project opens a second window, and
/// the one at the front the instant the launch returns is still the old one.
/// Placing that is placing the window the user was already happy with, while
/// the new one arrives seconds later wherever the editor felt like putting it.
#[cfg(target_os = "macos")]
pub fn editor_windows() -> usize {
    let Ok(name) = editor().map(|_| EDITOR_PROCESS) else {
        return 0;
    };
    let out = Command::new("osascript")
        .args(["-l", "JavaScript", "-e", COUNT_SCRIPT])
        .arg(name)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    out.ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
        .unwrap_or(0)
}

#[cfg(not(target_os = "macos"))]
pub fn editor_windows() -> usize {
    0
}

/// Open `dir` in the editor and put the two windows side by side, as one act.
///
/// One at a time, process-wide. Three of these at once are three scripts
/// moving the same two windows, each undoing the last and each retrying
/// because the others keep changing what it just read — which is how three
/// requests produce one window placed twice and two left at their default
/// size. Serialising them costs the third request the time of the first two,
/// and that is the correct price.
pub fn open_beside(dir: &Path, share: u32, of: u32) -> Result<Opened, DesktopError> {
    let _one_at_a_time = placing()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Counted inside the lock: a count taken before waiting for someone else's
    // placement is a count of a different world.
    let before = editor_windows();
    open_editor(dir)?;
    match tile_after(before, share, of) {
        Ok(()) => Ok(Opened::Placed),
        // It opened. That is what was asked for, and the windows not moving is
        // worth a line but is not a failure.
        Err(e) => Ok(Opened::NotPlaced(e.to_string())),
    }
}

/// What [`open_beside`] managed. Launching and placing fail separately.
#[derive(Debug)]
pub enum Opened {
    Placed,
    NotPlaced(String),
}

fn placing() -> &'static std::sync::Mutex<()> {
    static PLACING: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    PLACING.get_or_init(Default::default)
}

/// The editor as the window server names its process.
const EDITOR_PROCESS: &str = "Code";

#[cfg(target_os = "macos")]
const COUNT_SCRIPT: &str = r#"
function run(argv) {
  try { return String(Application('System Events').processes.byName(argv[0]).windows().length); }
  catch (e) { return '0'; }
}
"#;

/// Bring the editor's window for `dir` to the front of the editor's own
/// windows, leaving it exactly where it is.
///
/// The window is raised, not the application: activating the editor would take
/// the keyboard away from the terminal the user is typing in, and switching
/// session is not asking to leave. Within its own app the window comes to the
/// front, which — with the two tiled side by side and neither on top of the
/// other — is all that "bring it forward" can honestly mean.
///
/// `Ok(false)` when the editor has no window for that directory. That is the
/// ordinary case and not a failure: most sessions have no editor open.
#[cfg(target_os = "macos")]
pub fn raise_editor_for(dir: &Path) -> Result<bool, DesktopError> {
    let titles = editor_titles();
    let Some(title) = window_for(&titles, dir) else {
        return Ok(false);
    };
    let out = Command::new("osascript")
        .args(["-l", "JavaScript", "-e", RAISE_SCRIPT])
        .arg(EDITOR_PROCESS)
        .arg(title)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| DesktopError::Placement(format!("osascript: {e}")))?;
    Ok(String::from_utf8_lossy(&out.stdout).trim() == "ok")
}

#[cfg(not(target_os = "macos"))]
pub fn raise_editor_for(_dir: &Path) -> Result<bool, DesktopError> {
    Ok(false)
}

/// The titles of the editor's windows, front to back.
#[cfg(target_os = "macos")]
fn editor_titles() -> Vec<String> {
    let Ok(out) = Command::new("osascript")
        .args(["-l", "JavaScript", "-e", TITLES_SCRIPT])
        .arg(EDITOR_PROCESS)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .filter(|t| !t.is_empty())
        .collect()
}

/// Which window belongs to `dir`, by the only thing the window server will say
/// about it.
///
/// Matching on a title is a guess, and it is the guess the editor invites: it
/// titles a window `file — folder`, or `folder` alone, and puts its own notes
/// in between — `file (Working Tree) — folder`. So the title is split on the
/// dash and a segment has to *be* the directory's name rather than merely
/// contain it: a folder called `src` would otherwise match every window with a
/// file from some `src` open.
///
/// Deliberately the last resort and not the first. It decides which window to
/// *raise*, never which to move: a wrong guess here brings the wrong project
/// forward, which is visible and undone by looking away; a wrong guess about
/// what to place moves a window somebody had arranged.
fn window_for<'a>(titles: &'a [String], dir: &Path) -> Option<&'a str> {
    let name = dir.file_name()?.to_str()?;
    titles
        .iter()
        .find(|t| {
            t.split('\u{2014}')
                .flat_map(|part| part.split(" - "))
                .any(|part| part.trim() == name)
        })
        .map(String::as_str)
}

#[cfg(target_os = "macos")]
const TITLES_SCRIPT: &str = r#"
function run(argv) {
  var se = Application('System Events');
  try {
    return se.processes.byName(argv[0]).windows().map(function (w) {
      try { return String(w.name()); } catch (e) { return ''; }
    }).join('\n');
  } catch (e) { return ''; }
}
"#;

/// Raised by an exact title rather than by an index: the list was taken a
/// moment ago, and a window that closed in between would make an index name
/// somebody else's window.
#[cfg(target_os = "macos")]
const RAISE_SCRIPT: &str = r#"
function run(argv) {
  var se = Application('System Events');
  var ws;
  try { ws = se.processes.byName(argv[0]).windows(); } catch (e) { return 'no'; }
  for (var i = 0; i < ws.length; i++) {
    var t = '';
    try { t = String(ws[i].name()); } catch (e) { continue; }
    if (t !== argv[1]) continue;
    // The window, not the application: `frontmost` would take the keyboard
    // away from the terminal the user is typing in.
    try { ws[i].actions.byName('AXRaise').perform(); return 'ok'; } catch (e) { return 'no'; }
  }
  return 'no';
}
"#;

/// The application hosting this process, named as the window server names it.
///
/// Walks up to the last ancestor before `launchd`, which for anything started
/// from a shell is the terminal emulator, and takes the file name of its
/// executable: `Terminal`, `iTerm2`, `ghostty`. That is exactly the string
/// System Events uses for a process, so it needs no table of known terminals —
/// a terminal nobody has heard of works the same as the two that ship.
#[cfg(target_os = "macos")]
pub fn host_application() -> Option<String> {
    let out = Command::new("ps")
        .args(["-Ao", "pid=,ppid=,comm="])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    outermost_ancestor(&text, std::process::id() as i32)
}

/// The last ancestor of `start` before `launchd`, as a bare process name.
///
/// Split out from the `ps` call so it can be tested against real output —
/// which right-aligns its columns, so the fields are separated by a run of
/// spaces and not by one. Splitting on a single whitespace character silently
/// returns an empty second field and everything downstream reads the wrong pid.
fn outermost_ancestor(ps_output: &str, start: i32) -> Option<String> {
    let mut parents = std::collections::HashMap::new();
    for line in ps_output.lines() {
        let mut it = line.split_whitespace();
        let (Some(pid), Some(ppid), Some(comm)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        let (Ok(pid), Ok(ppid)) = (pid.parse::<i32>(), ppid.parse::<i32>()) else {
            continue;
        };
        parents.insert(pid, (ppid, comm.to_string()));
    }

    let mut pid = start;
    let mut last = None;
    // Bounded: a cycle in the table would otherwise be an infinite loop, and
    // this table is read from another program's output.
    for _ in 0..64 {
        let Some((ppid, comm)) = parents.get(&pid) else {
            break;
        };
        last = Some(comm.clone());
        if *ppid <= 1 {
            break;
        }
        pid = *ppid;
    }
    last.and_then(|path| {
        Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
    })
}

#[cfg(not(target_os = "macos"))]
pub fn host_application() -> Option<String> {
    None
}

/// Give the editor the left `share`/`of` of the screen and this terminal the
/// rest of it.
///
/// The screen is the one the terminal is already on, not the main one: someone
/// with two monitors asked for this on the monitor they are looking at. Which
/// screen that is, for a window straddling two of them, is decided by where
/// its centre falls — for two screens side by side that is also the one
/// holding most of it, since the split is a single line.
///
/// Blocks for as long as it takes the editor's window to exist — it may have
/// been launched a moment ago — so never call it on the render thread.
#[cfg(target_os = "macos")]
pub fn tile(share: u32, of: u32) -> Result<(), DesktopError> {
    tile_after(usize::MAX, share, of)
}

/// The same, waiting for a window the editor does not have yet.
///
/// `existing` is how many windows it had before it was asked to open one. The
/// placement waits for one more than that before touching anything, so it
/// moves the window that was just asked for rather than the one that happened
/// to be at the front. `usize::MAX` means "do not wait", for a caller that has
/// no before to compare against.
#[cfg(target_os = "macos")]
pub fn tile_after(existing: usize, share: u32, of: u32) -> Result<(), DesktopError> {
    let terminal = host_application().ok_or_else(|| {
        DesktopError::Placement("cannot tell which terminal this is running in".into())
    })?;
    // Arguments, never interpolation: these names come from another program's
    // output, and a script that pastes them into itself is a script that runs
    // whatever they happen to contain.
    let out = Command::new("osascript")
        .arg("-l")
        .arg("JavaScript")
        .arg("-e")
        .arg(TILE_SCRIPT)
        .arg(&terminal)
        .arg(EDITOR_PROCESS)
        .arg(share.to_string())
        .arg(of.to_string())
        .arg(existing.to_string())
        .output()
        .map_err(|e| DesktopError::Placement(format!("osascript: {e}")))?;

    let answer = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    placement_result(&answer, &stderr)
}

/// What the script said, turned into something worth showing a user.
///
/// Separate so the one failure that actually happens can be tested against the
/// words macOS really uses. Everything here is reached only when the editor has
/// already opened, so none of it is a failure of the thing that was asked for —
/// the windows just did not move.
#[cfg(target_os = "macos")]
fn placement_result(answer: &str, stderr: &str) -> Result<(), DesktopError> {
    if answer == "ok" {
        return Ok(());
    }
    let why = if answer.is_empty() { stderr } else { answer };
    // The overwhelmingly common failure, and the only one the user can fix.
    // macOS words it "not allowed assistive access", and AppleScript reports it
    // as error -1719 or -25211 depending on which layer refuses first.
    let lower = why.to_lowercase();
    if lower.contains("assistive access") || lower.contains("not allowed") || why.contains("-1719")
    {
        return Err(DesktopError::Placement(
            "opened, but placing windows needs Accessibility \u{2014} give your \
             terminal permission in System Settings \u{203a} Privacy & Security \u{203a} Accessibility"
                .into(),
        ));
    }
    Err(DesktopError::Placement(format!(
        "opened, but the windows could not be placed: {why}"
    )))
}

#[cfg(not(target_os = "macos"))]
pub fn tile(_share: u32, _of: u32) -> Result<(), DesktopError> {
    Err(DesktopError::Unsupported)
}

#[cfg(not(target_os = "macos"))]
pub fn tile_after(_existing: usize, _share: u32, _of: u32) -> Result<(), DesktopError> {
    Err(DesktopError::Unsupported)
}

/// Cocoa measures screens from the bottom left of the main one; System Events
/// measures windows from the top left of it. Everything below is in the second
/// system, so the screen rectangles are converted once on the way in and the
/// arithmetic afterwards is ordinary.
#[cfg(target_os = "macos")]
const TILE_SCRIPT: &str = r#"
function run(argv) {
  ObjC.import('AppKit');
  var termName = argv[0], editorName = argv[1];
  var share = parseInt(argv[2], 10), of = parseInt(argv[3], 10);
  // How many windows the editor had before it was asked for another one.
  var existing = parseInt(argv[4], 10);
  if (isNaN(existing)) existing = -1;
  var se = Application('System Events');

  // A specifier, resolved again on every use. A window object held across a
  // resize goes stale: the next thing asked of it fails with -1728, "Can't get
  // object", and takes the rest of the placement with it — which is what left
  // the terminal moved but never resized.
  function win(name) { return se.processes.byName(name).windows[0]; }
  function windows(name) {
    try { return se.processes.byName(name).windows().length; } catch (e) { return 0; }
  }

  // The editor may have been launched a heartbeat ago; wait for its window
  // rather than reporting a failure that fixes itself. And when it already had
  // windows, wait for the *new* one: a second project opens a second window,
  // which arrives seconds after the launch returns, and until it does the
  // window at the front is the one the user was already happy with.
  // The editor is also entitled to reuse a window instead of opening one, and
  // then no new window is ever coming. Its title changing is the other end of
  // the same wait — used only to stop waiting, never to choose a window.
  function frontTitle() {
    try { return String(win(editorName).name()); } catch (e) { return ''; }
  }
  var wanted = existing < 0 ? 1 : existing + 1;
  var was = frontTitle();
  for (var i = 0; i < 60 && windows(editorName) < wanted && frontTitle() === was; i++) {
    // Foundation, not the scripting additions: `delay` belongs to Standard
    // Additions, which `osascript -l JavaScript -e` does not always have.
    $.NSThread.sleepForTimeInterval(0.1);
  }
  // Not an error when it simply reused one — the editor is entitled to decide
  // that — so what follows places whatever is at the front.
  if (windows(editorName) === 0) return 'the editor never showed a window';
  if (windows(termName) === 0) return 'no window found for ' + termName;

  // Assert one property until it takes. The window server applies these
  // asynchronously and silently drops what it cannot honour yet, so a single
  // assignment is a request and not a result. `tolerance` because a window is
  // entitled to argue: Terminal rounds its height to whole rows. Ending up
  // somewhere else anyway is not reported — the windows are placed for the
  // user's benefit and roughly right is worth more than an error message.
  // Never managing to assign at all is the real failure, and the only one
  // they can act on: it is what a missing Accessibility permission looks like.
  // What a window is allowed to argue about: Terminal rounds its height to
  // whole rows, and a screen's usable top is a menu bar lower than its frame.
  var TOLERANCE = 40;

  function apply(name, prop, want, tolerance) {
    var applied = false, err = 'no window';
    for (var i = 0; i < 20; i++) {
      try { win(name)[prop] = want; applied = true; } catch (e) { err = String(e); }
      var got = null;
      try { got = win(name)[prop](); } catch (e) { err = String(e); }
      if (got !== null) {
        if (Math.abs(got[0] - want[0]) <= tolerance && Math.abs(got[1] - want[1]) <= tolerance) {
          return null;
        }
      }
      $.NSThread.sleepForTimeInterval(0.05);
    }
    return applied ? null : 'could not place ' + name + ': ' + err;
  }

  // Position and size argue with each other, and each one is only true until
  // the other is asserted: a resize at the edge of a screen is pushed back by
  // the window server, and a move *after* a resize quietly costs the window
  // part of its height. Asserting them in some clever order does not settle
  // it — what settles it is asking for both and then checking both, together,
  // until the pair holds at once.
  function near(got, want) {
    return got !== null
        && Math.abs(got[0] - want[0]) <= TOLERANCE
        && Math.abs(got[1] - want[1]) <= TOLERANCE;
  }

  function place(name, x, y, w, h) {
    var p = null, s = null;
    for (var pass = 0; pass < 4; pass++) {
      var why = apply(name, 'position', [x, y], TOLERANCE)
             || apply(name, 'size', [w, h], TOLERANCE);
      if (why !== null) return why;
      try { p = win(name).position(); s = win(name).size(); } catch (e) { p = null; s = null; }
      if (near(p, [x, y]) && near(s, [w, h])) return null;
    }
    // Said out loud, with both numbers. A window that would not go where it
    // was put used to report success, which is how a placement that visibly
    // did not happen came back as 'opened' and nothing else.
    return name + ' would not take ' + w + '\u{d7}' + h + ' at ' + x + ',' + y
         + ' \u{2014} it is ' + (s === null ? '?' : s[0] + '\u{d7}' + s[1])
         + ' at ' + (p === null ? '?' : p[0] + ',' + p[1]);
  }

  var screens = $.NSScreen.screens;
  var mainH = screens.objectAtIndex(0).frame.size.height;
  var rects = [];
  for (var i = 0; i < screens.count; i++) {
    var f = screens.objectAtIndex(i).visibleFrame;
    rects.push({ x: f.origin.x, y: mainH - (f.origin.y + f.size.height),
                 w: f.size.width, h: f.size.height });
  }

  var p = win(termName).position(), s = win(termName).size();
  var cx = p[0] + s[0] / 2, cy = p[1] + s[1] / 2;
  var scr = rects[0];
  for (var i = 0; i < rects.length; i++) {
    var r = rects[i];
    if (cx >= r.x && cx < r.x + r.w && cy >= r.y && cy < r.y + r.h) { scr = r; break; }
  }

  var left = Math.round(scr.w * share / of);
  var why = place(editorName, scr.x, scr.y, left, scr.h);
  if (why !== null) return why;

  // Where the editor actually landed, rather than where it was asked to go:
  // a screen's usable rectangle is not quite what `visibleFrame` says — a
  // second display carries its own menu bar, and the window server clamps to
  // it. Fitting the terminal to the editor's own top, height and right edge
  // takes that out of the arithmetic: whatever the first window was allowed,
  // the second one gets the rest of the row, exactly adjacent to it.
  var edP = win(editorName).position(), edS = win(editorName).size();
  var x = edP[0] + edS[0];
  why = place(termName, x, edP[1], scr.x + scr.w - x, edS[1]);
  if (why !== null) return why;
  return 'ok';
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// The editor's own titling, in the shapes it really produces.
    #[test]
    fn a_window_is_found_by_the_folder_in_its_title() {
        let titles: Vec<String> = [
            "Welcome — /",
            "spa.gateway.feature.changes.log.yaml — back-office",
            "context-menu.component.tsx (Working Tree) — TimePulse",
            "plank",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        let found = |p: &str| window_for(&titles, Path::new(p));
        assert_eq!(found("/Users/x/prj/back-office"), Some(titles[1].as_str()));
        assert_eq!(found("/Users/x/prj/TimePulse"), Some(titles[2].as_str()));
        // A window titled by its folder alone.
        assert_eq!(found("/Users/x/prj/plank"), Some(titles[3].as_str()));
        assert_eq!(found("/Users/x/prj/nothing-here"), None);
    }

    /// A segment has to *be* the name, not contain it. Otherwise a session in
    /// any `src` raises whichever project happens to have a file open from one.
    #[test]
    fn a_folder_name_inside_another_word_is_not_a_match() {
        let titles: Vec<String> = ["main.rs — src-tauri", "index.ts — websrc"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(window_for(&titles, Path::new("/x/src")), None);
        assert_eq!(
            window_for(&titles, Path::new("/x/src-tauri")),
            Some(titles[0].as_str())
        );
    }

    /// Reusing a window throws away the folder that was in it. Someone who
    /// opens three projects wants three, and a flag that quietly closes the
    /// last one is a flag that loses their work in progress.
    #[test]
    fn the_editor_is_never_told_to_reuse_a_window() {
        let args = editor_args(Path::new("/tmp/project"));
        assert!(
            !args.iter().any(|a| a == "-r" || a == "--reuse-window"),
            "{args:?}"
        );
        assert_eq!(args, vec![std::ffi::OsString::from("/tmp/project")]);
    }

    #[test]
    fn an_absolute_editor_is_taken_as_given() {
        assert!(
            matches!(
                resolve_editor("/definitely/not/here"),
                Err(DesktopError::NoEditor(n)) if n == "/definitely/not/here"
            ),
            "a path that is not there must say so, not fall back to a search"
        );
        // And one that is there is used without consulting PATH at all.
        assert_eq!(
            resolve_editor("/bin/sh").ok(),
            Some(PathBuf::from("/bin/sh"))
        );
    }

    /// The message has to name the thing that was missing. "No editor" alone
    /// leaves the user with nowhere to go.
    #[test]
    fn a_missing_editor_names_itself_and_the_way_out() {
        let e = DesktopError::NoEditor("code".into()).to_string();
        assert!(e.contains("code"), "{e}");
        assert!(e.contains("DMAC_EDITOR"), "{e}");
    }

    /// A hand probe: what does this machine actually answer?
    ///
    /// **This rearranges the screen.** It is the real call, so it really does
    /// move the editor and this terminal into their halves — which is the only
    /// way to learn whether placement works here, and the reason it is ignored
    /// rather than merely slow. It also depends on a permission and on which
    /// applications happen to be running, neither of which a suite may assume.
    ///
    ///     cargo test -p dmac-desktop -- --ignored --nocapture
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn what_this_machine_says_about_placement() {
        println!("host application : {:?}", host_application());
        println!("editor           : {:?}", editor());
        println!(
            "tile(4, 5)       : {:?}",
            tile(4, 5).map_err(|e| e.to_string())
        );
    }

    /// The words are macOS's own, taken from a real refusal. A message that
    /// only said "placement failed" would leave the user with nowhere to go,
    /// and this is the failure everyone meets first.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_refused_permission_says_which_permission_and_where() {
        let e = placement_result(
            "",
            "execution error: osascript is not allowed assistive access. (-1719)",
        )
        .expect_err("that is a refusal");
        let msg = e.to_string();
        assert!(msg.contains("Accessibility"), "{msg}");
        assert!(msg.contains("System Settings"), "{msg}");
        assert!(
            msg.starts_with("opened, but"),
            "the editor did open; the message must not read as a failure: {msg}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ok_is_ok_and_anything_else_is_quoted_back() {
        assert!(placement_result("ok", "").is_ok());
        let e = placement_result("the editor never showed a window", "")
            .expect_err("not ok")
            .to_string();
        assert!(e.contains("never showed a window"), "{e}");
    }

    /// `delay` belongs to Standard Additions, and `osascript -l JavaScript -e`
    /// does not always have them: on this machine `Application.currentApplication()`
    /// answers "Message not understood" (-1708) and the exception takes the
    /// whole placement with it — so the windows never moved, and the only time
    /// it bit was the one that matters, when the editor was still starting and
    /// the script had to wait for its window.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_script_waits_without_the_scripting_additions() {
        assert!(
            TILE_SCRIPT.contains("NSThread.sleepForTimeInterval"),
            "the wait has to be one that always exists"
        );
        for forbidden in [
            "std.delay",
            "includeStandardAdditions",
            "currentApplication",
        ] {
            assert!(
                !TILE_SCRIPT.contains(forbidden),
                "`{forbidden}` is not available under `osascript -e`"
            );
        }
    }

    /// The names of the applications come out of `ps`. Interpolating them into
    /// the script would be handing another program's output to an interpreter.
    #[test]
    fn the_script_takes_its_names_as_arguments() {
        #[cfg(target_os = "macos")]
        {
            assert!(
                TILE_SCRIPT.contains("function run(argv)"),
                "not argument-driven"
            );
            assert!(
                !TILE_SCRIPT.contains("{}") && !TILE_SCRIPT.contains("{0}"),
                "the script has an interpolation hole in it"
            );
        }
    }

    /// `ps` right-aligns its columns, so the fields are a run of spaces apart.
    /// Splitting on one whitespace character returns an empty second field and
    /// every pid read after it is wrong.
    #[test]
    fn the_process_table_is_parsed_the_way_ps_actually_prints_it() {
        let table = "    1     0 /sbin/launchd\n\
                     5837     1 /System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal\n\
                     5838  5837 /usr/bin/login\n\
                     5839  5838 -zsh\n\
                    85418  5839 /Users/x/prj/target/release/dmac\n";
        assert_eq!(
            outermost_ancestor(table, 85418).as_deref(),
            Some("Terminal"),
            "the terminal is the last ancestor before launchd"
        );
        assert_eq!(
            outermost_ancestor(table, 424242),
            None,
            "a pid that is not in the table has no ancestry"
        );
    }

    /// A table that points at itself must not spin for ever. It is another
    /// program's output, so it does not have to make sense.
    #[test]
    fn a_cycle_in_the_table_terminates() {
        let table = "10 11 /a/one\n11 10 /b/two\n";
        assert!(outermost_ancestor(table, 10).is_some());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_host_application_is_a_bare_name() {
        let Some(host) = host_application() else {
            return; // a test runner with no GUI ancestor is a fair answer
        };
        assert!(!host.contains('/'), "{host} is a path, not a process name");
        assert!(!host.is_empty());
    }
}
