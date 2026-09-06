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

/// Open `dir` in the editor, reusing a window rather than adding one.
///
/// `-r` is the difference between "show me this folder" and "give me a fifth
/// window": VS Code reuses the last active one, and if that window already has
/// this folder open it simply comes forward. Which is the asked-for behaviour
/// exactly — the editor already knows whether it has the folder, and asking it
/// is more reliable than any window title we could match on ourselves.
pub fn open_editor(dir: &Path) -> Result<(), DesktopError> {
    let editor = editor()?;
    Command::new(editor)
        .arg("-r")
        .arg(dir)
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
/// with two monitors asked for this on the monitor they are looking at.
///
/// Blocks for as long as it takes the editor's window to exist — it may have
/// been launched a moment ago — so never call it on the render thread.
#[cfg(target_os = "macos")]
pub fn tile(share: u32, of: u32) -> Result<(), DesktopError> {
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
        .arg("Code")
        .arg(share.to_string())
        .arg(of.to_string())
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
  var se = Application('System Events');
  var std = Application.currentApplication();
  std.includeStandardAdditions = true;

  function frontWindow(name) {
    try {
      var ws = se.processes.byName(name).windows();
      return ws.length ? ws[0] : null;
    } catch (e) { return null; }
  }

  // The editor may have been launched a heartbeat ago; wait for its window
  // rather than reporting a failure that fixes itself.
  var ed = null;
  for (var i = 0; i < 60 && ed === null; i++) {
    ed = frontWindow(editorName);
    if (ed === null) std.delay(0.1);
  }
  if (ed === null) return 'the editor never showed a window';
  var term = frontWindow(termName);
  if (term === null) return 'no window found for ' + termName;

  var screens = $.NSScreen.screens;
  var mainH = screens.objectAtIndex(0).frame.size.height;
  var rects = [];
  for (var i = 0; i < screens.count; i++) {
    var f = screens.objectAtIndex(i).visibleFrame;
    rects.push({ x: f.origin.x, y: mainH - (f.origin.y + f.size.height),
                 w: f.size.width, h: f.size.height });
  }

  var p = term.position(), s = term.size();
  var cx = p[0] + s[0] / 2, cy = p[1] + s[1] / 2;
  var scr = rects[0];
  for (var i = 0; i < rects.length; i++) {
    var r = rects[i];
    if (cx >= r.x && cx < r.x + r.w && cy >= r.y && cy < r.y + r.h) { scr = r; break; }
  }

  var left = Math.round(scr.w * share / of);
  ed.position = [scr.x, scr.y];
  ed.size = [left, scr.h];
  term.position = [scr.x + left, scr.y];
  term.size = [scr.w - left, scr.h];
  return 'ok';
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

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
