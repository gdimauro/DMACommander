# Changelog

Notable changes, newest first. Versions follow the policy in
[docs/VERSIONING.md](docs/VERSIONING.md).

## Unreleased

### Added
- **A spectrum analyser that listens to the room.** Log-spaced frequency bands,
  because linear FFT bins put everything musical in the leftmost tenth of the
  screen; fast attack and slow decay, because a transient has to arrive
  instantly and leave slowly; and eight block glyphs per row for sub-cell
  resolution, so quiet passages still move. The microphone is opened when the
  effect starts and released when it stops — a file manager holding the
  microphone open while you are not looking at it is not one anybody should
  trust. No device, no permission or an unsupported format each say so on screen,
  with what to do about it.
- **The session rail is a manager, not just a display.** `Shift-Tab` or `F11`
  opens it *and* hands it the keyboard: `↑↓` move the highlight without switching
  (you look before you leap), `Enter` switches, `n` creates, `r` renames, `d`
  closes, and a bare digit jumps — the numbers are on screen right there, so
  demanding a modifier would be perverse. `Alt+1..9` still works from the panels.
  Renaming refuses an empty or duplicate name and keeps the text so it can be
  corrected rather than retyped.
- **A real shell in every session.** `dmac-pty` hosts a child on a pseudo-terminal
  (`portable-pty`, so ConPTY on Windows) and interprets its output with a proper
  terminal emulator (`vt100`). `Ctrl-O` swaps between the panels and the shell —
  the Norton Commander binding, now with an actual shell to reveal. A command
  typed on the command line runs there and switches to the view, because a
  command whose output you cannot see has not really run.

  The shell starts in the active panel's directory, loads your rc files, and is
  spawned on first use rather than at startup. While it is showing it owns the
  keyboard, with exactly one binding reserved to get back out — otherwise half
  the keys a shell needs would be eaten by the file manager. Keys are encoded the
  way a terminal encodes them, so Ctrl-C interrupts, Ctrl-D ends input, arrows
  reach history and Alt-b moves by word.

  A hosted process never outlives its session, and a shell that dies is replaced
  rather than silently accepting keys forever.
- **Asteroids**, played by the computer. It is an arcade attract mode, so unlike
  Snake it belongs in the idle rotation: it needs nobody watching. Pressing any
  key still gives you your file manager back — taking over is deliberate, on
  Space. Drawn as real wireframe vectors, which needed line and polygon drawing
  on the effect canvas.
- **Multiple live sessions.** `dmac-session` holds several workspaces at once,
  each with its own panels, focus, command line and current directory. They are
  live, not swapped: switching does not save one and load another, so nothing
  reloads and each session keeps the listing and cursor position it had. This is
  what lets the rail stand in for a window switcher rather than being a bookmark
  list.
- **Session rail** down the left edge. Collapsed it is three columns of coloured
  dots, so you can always see how many sessions exist and which one you are in;
  `Shift-Tab` expands it to names, positions and paths. It *pushes* the panels
  rather than covering them — both stay readable, and a file can eventually be
  dragged from a panel onto a session, which is the natural route to the
  cross-session clipboard.
  `Alt+1..9` jumps by position, `Ctrl-N` opens a session on the current
  directory, `Ctrl-W` closes one, `Ctrl-PageUp/PageDown` cycles, and clicking a
  row in the rail switches to it. Closing the last session is refused and points
  at F10 rather than quietly becoming a way to quit.
- **Shift + cursor keys select a range.** Anchored rather than toggle-and-move,
  so backing off shrinks the selection instead of stamping a second toggle over
  it — the same way the mouse drag already behaved, and two gestures that select
  a range should not disagree about what reversing means. Marks made earlier with
  Ins survive a span shrinking back over them, `..` is never swept in, and the
  status line shows the running count. Shift+Up/Down, Shift+PageUp/PageDown and
  Shift+Home/End all extend; any plain movement or mouse click ends the gesture.
- `run-release.sh` and `run-debug.sh`. The debug one redirects stderr to a log
  file, because anything written to stderr while a TUI is running corrupts the
  display.
- `F11` also toggles the session rail. Shift+Tab is claimed by several terminals
  (Warp among them) before it reaches the application.
- `--cursor <style>` — `blinking-block` (default), `blinking-bar`,
  `blinking-underline`, `steady-block`, or `software`. See *Fixed* below.
- Startup splash showing version, build number, commit, build time, rustc and
  target. Drawn over the panels rather than instead of them, so the app never
  looks like it is still loading when it is already usable. Any key dismisses it,
  and that key is swallowed. `--no-splash` skips it.
- Build identity captured at compile time (`dmac-config::build_info`), surfaced
  by `--version`, `--build-info` and the splash. The commit count serves as a
  monotonic build number, and a dirty working tree is marked so a SHA is never
  misleading.
- `docs/VERSIONING.md` and this changelog.

### Fixed
- **Directory listings could land in the wrong session.** Results were routed by
  panel alone, and each session has its own generation counter — so a stale
  result from one session could carry a generation that happened to match
  another's and be painted into a workspace that never asked for it. Updates now
  carry the session id, and a result whose session has been closed is dropped.
- **A panel opened on a relative path had no `..` row.** `Path::parent` of a
  one-component relative path is the empty path, so `dmac docs` gave a panel you
  could descend from but never climb out of. Starting directories are now made
  absolute and canonical up front, which is what a file manager should be showing
  anyway, and an empty parent is treated as no parent.
- **Typing `-`, `+` or `*` on the command line did nothing.** They were bound to
  the Norton selection-mask actions before focus was consulted, so every hyphen
  in a command was silently eaten — `uname -s` arrived as `uname s`. They are now
  resolved under panel focus only.
- **Life looked like television snow.** It was seeded with uniform random soup,
  which never resolves into anything you can follow. It now starts from known
  patterns on a mostly-empty board — gliders, spaceships, a pulsar, the
  R-pentomino, the acorn, and a Gosper gun that emits a glider stream forever.
- **The command-line cursor did not blink** in some terminals. The shape was
  asked for once at startup with DECSCUSR, and a terminal that resets it — or
  overrides it with its own cursor preference — left the cursor steady with
  nothing to explain why. It is now re-asserted on every frame that actually
  shows a cursor. Where the terminal ignores the request entirely,
  `--cursor software` stops asking and draws the cursor as an inverted cell,
  blinking on a 530ms phase, which works everywhere.
- **Startup took 2.0 seconds** in any terminal without the kitty keyboard
  protocol. `supports_keyboard_enhancement()` queries the terminal and waits for
  a reply that such terminals never send, burning the full 2s timeout in exactly
  the case where the answer is "no". The flags are now pushed without asking —
  an ordinary CSI sequence, discarded by terminals that do not implement it.
  First frame went from 2004ms to 4ms, against an 80ms budget. Set
  `DMAC_NO_KEYBOARD_ENHANCEMENT=1` to opt out.

## 0.1.0 — 2026-09-05

Initial commit. Two panels with streaming listings, the Norton keymap, an
explicit focus model, full mouse support, five screensavers plus Snake, a VFS
trait with a local backend, and panic-safe terminal restore.
