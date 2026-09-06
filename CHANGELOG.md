# Changelog

Notable changes, newest first. Versions follow the policy in
[docs/VERSIONING.md](docs/VERSIONING.md).

## Unreleased

### Added
- **Switching session brings that session's editor forward.** If a window is
  open on the directory the session is showing, it comes to the front of the
  editor's windows — the per-window Alt-Tab macOS does not have. Raised, never
  moved: the placement is something you asked for once, when you opened the
  directory, and a window you have dragged since is where you wanted it.

  The *window* is raised and not the application, so the keyboard stays in the
  terminal you are typing in — switching session is not a request to leave it.
  The window is found by its title, split on the dash the editor puts between
  the file and the folder, with a segment having to *be* the directory's name
  rather than merely contain it: a session in some `src` must not raise whatever
  project happens to have a file from one open. A guess, and deliberately one
  that only ever decides what to raise — never what to move.
- **F9 lists the other sessions, and goes to them.** Under the utilities, behind
  a separator, one row per session you are not in — answering to the same digit
  the rail gives it, which is the digit `Alt` already jumps on. The rail has had
  this list all along, on a key that is one more thing to know; a list that
  exists somewhere other than the menu people reach for is a list nobody finds.
- **F9 starts the agent, too.** `c` in the utilities runs `claude` in this
  session's shell and shows it. Typed at the shell rather than spawned beside
  it, because the shim on `PATH` is what hands the agent this session's
  conversation and the commander's own MCP description, and it only gets to do
  that for something the shell runs. A shell that is busy is left alone and says
  so: a line typed at something already running is not a command, it is a
  sentence handed to whatever has the keyboard.
- **F9 opens the editor where you already are.** The utilities menu gained one
  entry that does something rather than producing something: `o` opens the
  active panel's directory in the editor and tiles the two windows, the same as
  `F5` in the history — for when you are already looking at the place you want
  opened and going through the history to name it would be absurd. The menu
  still touches nothing itself: it returns the deed, and the application
  performs it on a thread that is not drawing.
- **`.dmac-session`: a checkout can name the session it belongs to.** The third
  step of the startup resolution — after `--session` and `$DMAC_SESSION` — walks
  up from the working directory and takes the first one it finds, nearest first,
  so `cd` into a project from anywhere and its panels, shells and conversation
  come back with it. The file is one line, `#` comments and blank lines skipped.

  It is treated as content and not as configuration, because it arrives with a
  `git clone` like everything else in the tree: a name with a control character,
  a separator, or more than 64 characters in it is refused, and refused *out
  loud* — the file is named on stderr before the screen is taken over, because a
  session silently called `main` when the tree asked for something else is
  someone looking at the wrong workspace and not knowing why.
- **A hosted shell can be read back and copied out of.** Two thousand lines are
  kept above the top of the pane. `Shift-PgUp`/`PgDn` walks through them and the
  wheel scrolls three lines a notch; `Shift` with the arrows, `Home`/`End` and
  the page keys selects, from wherever the shell's own cursor is, and
  `Ctrl-Shift-C` copies. Holding the selection past the top of the pane scrolls
  the view and grows the selection with it, so what you copy is the whole of what
  you selected and not the part that happened to be on screen. Typing snaps back
  to the live screen the way every terminal does, and `Esc` means "back to live"
  only while there is something to come back from — otherwise it reaches the
  program, which needs it. `Ctrl-Shift` looks, `Shift` selects: the quick keys
  need a terminal that reports modifiers, and the ones that work everywhere cover
  all of it. See [docs/TERMINAL-KEYS.md](docs/TERMINAL-KEYS.md).
- **The session rail has two widths, and both are yours.** One for resting and
  one for open, remembered separately and kept across restarts. Drag its
  right-hand edge, or open it with `Ctrl-T` and use `Left`/`Right` or `-`/`+`.
  Widen the resting strip past eight columns and it stops being dots and shows
  names and paths all the time; drag it to nothing and it disappears, with
  `Ctrl-T` still opening it. `--rail-width` and `--rail-collapsed-width` set both
  from the command line, which is how you undo a drag that went too far.
- **[docs/TERMINAL-KEYS.md](docs/TERMINAL-KEYS.md): which keys your terminal can
  actually send.** Not a preference and not a bug — `Ctrl-H`, `Backspace` and
  `Ctrl-Shift-H` are the same byte in a terminal that does not report modifiers,
  and the information never left the keyboard driver. The page says which keys
  are safe everywhere, which need the kitty keyboard protocol, and what to press
  instead: `Ctrl-O` chords and `F9`/`F12` need nothing at all.
- **DMACommander is an MCP server.** An agent hosted in a session can see both
  panels, the directory history, the sessions and the screen itself, through ten
  tools that follow one rule: read freely, write visibly. Nothing to install —
  starting a shell writes the configuration and the shim hands it to `claude`.
  `dmac --mcp <socket>` is the pipe. See [docs/MCP.md](docs/MCP.md).
- **Ctrl-H: the directory history.** Everywhere the panels have been, kept across
  sessions and restarts, read three ways — newest first, most used anywhere, or
  this session only — on F1/F2/F3, with the bar at the bottom becoming the list's
  own. Typing filters, fuzzily, with the matched characters highlighted.
- **Backspace leaves the directory.** The quick search only owns the key while it
  has something to delete.
- **A launcher, an app bundle and an icon.** `packaging/install.sh` puts `dmac`
  on the PATH and DMACommander in the Dock, on the Desktop and in
  `~/Applications`, preferring terminals that speak the kitty keyboard protocol.
  `--uninstall` takes all of it back out.
- Hidden files move to Alt-H and Alt-period, where `mc` keeps them, because
  Ctrl-H is now the history.

### Changed
- **The shell goes where the panels go.** It used to follow only on the way in
  through `Ctrl-O`; now any move of the current session's active panel — the
  history, a jump, an agent — sends it after them. What is typed is still the
  shell's own call: a `cd` sent to something that is running is not a command,
  it is a line handed to whatever has the keyboard, so a busy shell is left
  alone and catches up at the next `Ctrl-O`.
- **The shell's border says where it is, not just what it is running.** The one
  view where you type commands was the one view that would not tell you where
  they would land. The directory is asked of the system rather than remembered:
  the commander only knows about the `cd`s it typed itself, and the whole point
  of a shell is that you type your own. When the panels have gone somewhere the
  shell could not follow, both are shown with an arrow between them — the
  answer to "did it come with me?" belongs on the screen.
- **F9 is the utilities, everywhere.** It used to be Norton's pull-down menu in
  the panels — which was never written, and answered "not implemented yet" —
  while meaning the utilities inside a hosted shell. One key with one meaning
  beats fidelity to a menu whose contents (panel modes, sort orders, options)
  live on their own keys here anyway. The F-key bar says `Utils`.
- **Opening a directory in the editor no longer takes away the one you had.**
  It used to pass `-r`, `--reuse-window`, chosen so that asking for four
  projects would not hand you four windows. That was the wrong trade: reusing a
  window means the folder that was in it is gone, which from the other side of
  the screen is indistinguishable from the editor having closed. Left to itself
  the editor does better than we could — a window already showing that folder
  comes forward, and otherwise it opens one, the way its owner configured it to.
- **Restarting asks before resuming an agent.** It used to spawn whatever the
  last run was hosting, silently, on every rebuild. Now it names each session,
  conversation and command line, and waits. Saying no loses nothing.

### Fixed
- **A second project was opened and then not placed.** Since the editor stopped
  reusing its window, asking for another one produces another window — and the
  placement was not waiting for it. It took the editor's front window the
  instant the launch returned, which is still the *old* one: the new window
  arrives seconds later, wherever the editor felt like putting it, and the one
  that got tiled was the one already sitting where the user wanted it. Opening
  three projects left one window placed three times and two at 1440×900.

  The count of windows is now taken before the launch and the placement waits
  for one more than that, or for the front window's title to change when the
  editor reused one instead. Opening and placing also became a single act taken
  one at a time: three requests at once were three scripts moving the same two
  windows, each undoing the last and each retrying because the others kept
  changing what it had just read.
- **The shim resumed conversations that were not there.** It asked the agent's
  own store whether a conversation existed and then, if a marker file said so,
  resumed anyway. But the marker is written when a conversation is *reserved*,
  and one nobody ever typed into leaves no transcript behind — so the marker
  outlives the conversation just as easily as the conversation outlives the
  marker. Resuming on it is not an empty conversation, it is a refusal to
  start. The store now decides; the marker only speaks when there is no store
  to consult. Tested by running the shim rather than by reading it: all of this
  lives in five lines of `sh`, and assertions about their text prove nothing
  about what `sh` does with them.
- **Resuming an agent replayed a command line that could not survive a shell.**
  What a session was hosting is observed through `ps`, which hands back an
  `argv` rejoined with spaces — every quote its author wrote already gone. The
  shim's own `--mcp-config` argument is a JSON object full of braces containing
  a path with a space in it, so replaying that line word for word gave `zsh` a
  glob to expand and got the whole command refused with `bad pattern`. Worse,
  anything lost from the tail of it turned `--resume <id>` into a bare
  `--resume`, which does not fail: it opens whichever conversation was most
  recent, and lands you somewhere you have never been with no idea why.

  What is replayed now is the *command* — the program by its bare name, plus
  everything the user chose, and nothing the shim added. The shim puts its own
  arguments back, naming this run's socket and this session's conversation,
  quoted properly because it is the one writing them. A resume flag can no
  longer come back without its id: the flag goes with it.
- **The agent shim was silently bypassed.** DMACommander puts its shim directory
  in front of `PATH` before the hosted shell starts — and then the shell reads
  its rc files, and a `.zshrc` doing `PATH="$HOME/.local/bin:$PATH"` puts that
  directory in front of ours. `claude` then started from the user's own `PATH`:
  no conversation of its own, and no MCP server, so the agent could not see the
  panels it was running inside. Nothing said so; the symptom was an absence.

  Startup now asks the user's own shell, interactively, where `claude` resolves,
  and offers to put a few lines at the end of the rc file that put the shim back
  in front. The lines are written in terms of `$DMAC_SHIM_DIR`, which is now in
  the environment too — so they stay correct for every future run and do nothing
  at all in a shell DMACommander did not start.
- **The tiled windows came out short, and said nothing about it.** Position and
  size argue with each other, and each is only true until the other is asserted:
  a resize at the edge of a screen is pushed back by the window server, and a
  move *after* a resize quietly costs the window part of its height — 175 pixels
  of it, measured. No ordering settles that. Both are now asked for and then
  checked *together*, up to four passes, and a window that will not take what it
  was given says so with both numbers instead of reporting success. The silence
  was the worse half of the bug: a placement that visibly did not happen came
  back as `opened` and nothing else.
- **F5 moved the terminal without resizing it.** The placement script held on to
  a window object across a resize. A System Events window reference goes stale
  the moment the window actually changes size: the next thing asked of it fails
  with -1728, which aborted the placement half-done and left the terminal moved,
  still its old size, and mostly off-screen. Every window is now re-resolved on
  each use, each move and resize is asserted until it takes rather than sent
  once and hoped for, and the terminal is fitted to where the editor actually
  landed instead of to `visibleFrame` — a second display carries its own menu
  bar, which is why the windows came up short.
- **A pseudo-terminal race.** Two threads calling `openpty` at once
  intermittently lose it, which is why the suite failed at random and why
  restoring several sessions together could fail to start a shell. Opening one is
  now serialised.

- **Sessions survive a restart.** They are written to one file, atomically
  (temp file plus rename, in the same directory so the rename stays atomic), and
  debounced — renaming a session one keystroke at a time should not mean one file
  write per keystroke. Launching with no arguments restores every session with
  its directories, sort orders, active panel and focus. `--no-session` opts out
  entirely rather than writing somewhere throwaway.

  Failures are forgiving in the direction that matters: a missing file is a first
  run, a corrupt one is moved aside and reported rather than blocking startup,
  and a file written by a newer version is refused and left alone instead of
  being silently downgraded. An unclean exit is announced on the next start,
  because a layout that looks subtly stale with no explanation is worse than one
  that says what happened. A session never comes back showing its shell: the
  process is gone, and a blank pane on startup would be alarming.
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
