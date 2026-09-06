# Execution plan

`ROADMAP.md` lists *what* gets built. This document is *in what order, by whom,
and how we know a milestone is actually finished.*

The organising principle: **every milestone ends with something you can use.**
No milestone delivers only internals. If a phase cannot be demonstrated by
opening the app and doing a thing, it is scoped wrong.

---

## Where we are

**M0 — Foundations: done.**

12 crates with enforced layering, 39 tests, clippy clean, 1.4 MB release binary.
Two panels list real directories with streaming loads, Norton keymap, natural
sort, orthodox selection, panic-safe terminal restore.

Three real bugs were caught by tests before a human saw them: an underflow on
narrow terminals, a date column two columns too narrow (which truncated *every*
row), and the `Ctrl-F3..F6` sort bindings shadowed by the bare F-key arms. This
is the working style the plan assumes — see *Definition of Done* below.

**M0.5 — Screensavers, the picker and the mouse: done.**

`dmac-fx` ships five effects (matrix, starfield, plasma, life, pipes) plus Snake
behind one backend-agnostic `Effect` trait, with idle detection that costs
literally nothing when inactive: `Screensaver::deadline()` returns `None` when
disabled, so the event loop contributes no timer at all rather than a timer that
never fires.

Games forced a design change that was worth making early: an effect returns an
`EffectControl` from `on_input`, so a screensaver dismisses on any key while a
game keeps its arrow keys. Games are excluded from `random` and from the idle
timer — being dropped into Snake because you went for coffee is not a feature.

Also landed:
- **Sorting moved to Shift+F3..F6** (was Ctrl). `Ctrl+Shift+F1` opens the
  screensaver picker, with `F12` as an equivalent because a good many terminals
  do not encode Ctrl+Shift with an F-key at all — a binding you cannot press is
  not a binding.
- **Mouse**: click to focus and move the cursor (an empty panel still takes
  focus), double-click to open, right-click to toggle a mark, left-drag to sweep
  a range, right-drag to sweep marks, wheel scrolls the panel *under the pointer*
  rather than the active one, and the F-key bar is clickable.
- Drag selection recomputes the range from the anchor on every move rather than
  accumulating, so a fast drag that outruns the event stream still selects every
  row it crossed.

This was originally scheduled behind M4 in the first draft of this plan, bundled
with the dock. That was wrong: only the *dock* needs the window manager — a
screensaver takes the whole screen and needs nothing that did not already exist.
They were blocked by association, not by dependency. Corrected, and worth
remembering as a failure mode when sequencing the rest.

**M2 — Sessions, hosting and continuity: most of it landed, out of order.**

The plan says the phases are sequential and M1 comes first. They did not happen
that way, and it is worth writing down why rather than quietly renumbering: what
the author actually needed was to keep an agent's conversation alive across a
restart, and none of that is blocked by copying files. M1 is still the gap it
always was — this program still cannot move a byte.

What is running:

- **`dmac-pty`** hosts a child in a real PTY with `vt100` behind it. Two thousand
  lines of scrollback that can be read back, selected across screenfuls and
  copied out. `dmac-desktop` launches, raises and tiles external windows.
- **Live sessions and the rail.** Several at once, never swapped; `Shift-Tab`,
  `F11` or `Ctrl-T`; two widths, dragged or given on the command line, both kept
  across restarts. Per-session view: panels or the hosted process, `Ctrl-O`
  between them.
- **Persistence.** One file, written atomically and debounced, quarantined when
  it is corrupt rather than blocking startup, with an unclean exit announced on
  the next run.
- **Startup resolution**, three of the five steps: `--session`, then
  `$DMAC_SESSION`, then a `.dmac-session` file walked up from the working
  directory. Auto-resume and the picker are still to come.
- **Agents come back with the session.** What a session was running is read
  from `ps`, saved verbatim, and offered back on the next start as a resume —
  the restart *asks* before reopening anything, and clears whatever is still
  holding that conversation first. Nothing is planted on `PATH` to arrange it:
  a shim there is defeated silently by any rc file that reorders `PATH`, which
  is why the one that used to be here is gone.
- **The commander is itself an MCP server**, so a hosted agent can see the
  panels, the history, the sessions and the screen. Ten tools, one socket per
  run, described to the agent inline so two commanders never share a file.
- **The directory history** (`Ctrl-H`), everywhere the panels have been, four
  ways to read it, fuzzy filter, and `F5` to open a row in the editor beside the
  commander.

What M2 still owes: the per-session on-disk model (`session.toml`,
`workspace.json` — today every session lives in one file), the startup picker,
auto-resume, the cross-session clipboard, and the acceptance gates that are
measurements rather than features — eight sessions against the RSS budget, a
grep of the written tree for secrets, and an older binary reading a newer file.

---

## Critical path

These four phases are sequential. Each one is blocked by the previous.

### M1 — The file manager you would actually use

**The gap this closes:** right now it renders directories. After M1 it moves
bytes, and you can stop reaching for `cp`.

| Work | Owner | Notes |
|---|---|---|
| `dmac-config`: platform paths (`directories`), TOML, keymap presets, themes | `rust-architect` + `ux-keeper` | Unblocks *everything* — nothing else should hardcode a path or a key |
| Minimal modal layer: confirmation, progress, input dialogs | `tui-engineer` | Deliberately **not** the full floating WM (that is M4). Modal-only is enough for M1 and 10× cheaper |
| File operation engine: scan → transfer → verify, resumable, cancellable | `fileops-engineer` | The dangerous crate. See its agent file before touching it |
| F5 copy · F6 move · F7 mkdir · F8 delete, wired to the engine | `fileops-engineer` + `ux-keeper` | Conflict dialog decided *before* the transfer starts |
| F3 viewer: text with syntax highlighting, hex mode, binary detection | `tui-engineer` | Image preview deferred to M4 |
| F4 editor: `edtui`/`tui-textarea`, save with atomic replace | `tui-engineer` | |
| Shell execution from the command line, output into the panel area | `tui-engineer` | `Ctrl-O` already reveals the space it draws into |
| Undo stack for file operations | `fileops-engineer` | Cheap now, near-impossible to retrofit later |

**Done when:** you can copy 10,000 files between panels with honest progress and
a working Esc; kill the process mid-copy and find no half-written file without a
`.dmac-part` suffix; delete to trash and restore; view and edit a file; run
`ls -la` on the command line and read the output.

**Acceptance gates:**
- Property test: copy-then-compare is byte-identical, including permissions and mtime.
- Kill-9 test during a copy leaves the destination tree in a state the app can explain on restart.
- The conflict dialog names the file, both sizes and both dates — never "Are you sure?".
- Budget: 100k-entry directory paints its first screen in < 100ms.

---

### M2 — Sessions, hosting and continuity

**The gap this closes:** you close the app and lose your place; and a shell or a
Claude session is something you leave the app to use. After M2 the commander
*hosts* them, and nothing is lost when you close it.

These are one milestone because they are one problem. A session is not "which
directories were open" — it is "what was I doing", and what you were doing
includes the Claude conversation running in the other pane. The
`AttachedSession` abstraction serves all of it.

**Sessions are live, not just saved.** Several run at once; `Shift-Tab` opens a
scrollable rail down the left listing them, and switching is instant because the
other sessions never stopped. Each one shows either its hosted process fullscreen
or the commander panels, and that choice is per-session state.

**The clipboard lives above sessions.** This is the structural consequence of
"copy in one session, paste in another": the clipboard cannot belong to a session
or to a panel. It is app-global, it holds a list of `VfsPath` plus an operation
(copy or cut), and because it holds VFS paths rather than local ones, copying
from an S3 session and pasting into an SFTP session is the same code path as
copying between two local directories. It also has to interoperate with the OS
clipboard, so a file copied here pastes into Finder or Explorer, and vice versa.

| Work | Owner |
|---|---|
| Session model on disk: `session.toml`, per-session config override, `workspace.json` | `session-engineer` |
| Startup resolution: `--session` → `$DMAC_SESSION` → `.dmac-session` → auto-resume → picker | `session-engineer` |
| Session picker TUI: name, description, last used, paths it will restore, clean/unclean exit | `session-engineer` + `ux-keeper` |
| Atomic debounced snapshots + crash recovery | `session-engineer` |
| **Concurrent live sessions**: several open at once, not one at a time | `session-engineer` |
| **Session rail on `Shift-Tab`**: a scrollable list down the left — running and saved, with what each is doing | `session-engineer` + `ux-keeper` |
| **Per-session view mode**: a session shows either its hosted process fullscreen, or the commander panels for navigating and copying | `tui-engineer` |
| **Cross-session clipboard**, files included: copy in one session, paste in another | `fileops-engineer` + `session-engineer` |
| **`dmac-pty`**: spawn a child in a PTY (`portable-pty`, ConPTY on Windows), emulate with `vt100`, render the grid | `tui-engineer` |
| **`dmac-desktop`**: launch, enumerate and *raise* external GUI windows; bind them to sessions (ADR 0003) | `tui-engineer` |
| **Per-window Alt-Tab that macOS does not have**: pick a session, its VS Code window comes forward | `session-engineer` + `ux-keeper` |
| **Host anything from inside**: a shell, `claude`, `plank`, an editor — launched from the panel or the command line | `tui-engineer` |
| **Foreground toggle**: one key brings the commander forward over a running hosted process, and back again | `tui-engineer` + `ux-keeper` |
| **Contextual F10**: returns to the hosted session when there is one to return to; quits when there is not. The F-key bar says which | `ux-keeper` |
| `AttachedSession` trait, with Claude Code as the first implementation | `session-engineer` |
| Claude reattach: **resume** the recorded conversation, never start a fresh one | `session-engineer` + `ai-integration-engineer` |
| Sessions record what they host and re-spawn it on reopen | `session-engineer` |
| Runtime session switching without restart | `session-engineer` |

**Done when:** you launch `claude` from inside the commander, work in it
fullscreen, hit one key to bring the panels forward, copy a file, hit F10 and land
back in the same conversation mid-scroll; `Shift-Tab` slides out the session rail
and you switch to another live session without either one restarting; you copy a
file in one session and paste it in another pointing at a different filesystem;
`dmac -s work` reopens with both panels, sort orders, selections and that Claude
session restored; `kill -9` followed by a restart offers the recovered snapshot
alongside the last clean one; a session directory copied to another machine opens
with warnings for missing paths rather than a crash.

**The toggle key — the one open design question.**

Whatever key brings the commander forward must never reach the hosted child, or
it stops being a toggle and becomes a keystroke the child ate. Three constraints
pull against each other: it has to be reachable, it has to be one the child does
not need, and it has to be sendable through when the user genuinely wants it.

The proposal: **`Ctrl-O`**, extended from what it already means here. In Norton
Commander `Ctrl-O` hides the panels to reveal the shell underneath; hosting is
the same gesture with a real process behind it, so the muscle memory transfers
instead of competing. Pressing it twice sends a literal `Ctrl-O` to the child, so
nothing is unreachable. It is rebindable like every other key, and the F-key bar
shows the current binding while a session is hosted.

`ux-keeper` owns the final call. It is written down here rather than decided in
code, because getting it wrong is the kind of thing that makes people stop using
a feature without being able to say why.

**Acceptance gates:**
- A recorded Claude session that no longer exists produces a visible, explicit
  message — never a silently-empty new conversation. This is the one failure
  mode that would destroy trust in the feature.
- The toggle key round-trips: commander → hosted → commander → hosted, with the
  child's screen intact and its scrollback unbroken. Tested against a real
  full-screen TUI child, not just a shell.
- A hosted process that exits reports how it exited and leaves its final screen
  readable, rather than silently vanishing.
- A cut whose source session is closed before the paste either completes or
  reports why it cannot — never a half-move.
- N live sessions cost N × (a PTY + a panel state), not N × a full app. Measured
  with eight sessions open against the RSS budget.
- No secret appears in any file under `sessions/`. Verified by grepping the
  written tree in a test.
- Schema carries a `version`; an older binary reading a newer file degrades
  rather than corrupting.

---

### M3 — Browse: the VFS opens up

**The gap this closes:** panels only show local disks. After M3 an S3 bucket, an
SSH host and a `.tar.gz` inside a `.zip` are all just folders.

| Work | Owner |
|---|---|
| Archive backend: zip, tar, gz/zstd/xz/bz2, 7z — browsed as directories | `vfs-engineer` |
| Nested VFS composition (`sftp://host/a.zip/inner.tar.gz/dir/`) | `vfs-engineer` |
| OpenDAL backend: S3, GCS, Azure, WebDAV, FTP, Drive, Dropbox, OneDrive | `vfs-engineer` |
| SFTP via `russh` for what OpenDAL cannot reach (agent auth, jump hosts) | `vfs-engineer` |
| Credentials in `keyring`; connection manager UI (`Alt-F1`/`Alt-F2`) | `vfs-engineer` + `ux-keeper` |
| Capability-aware UI: impossible actions greyed out, not failed late | `tui-engineer` |
| Cross-backend copy with streaming and constant memory | `fileops-engineer` |
| **Security review of extraction paths** — zip-slip is where this app earns a CVE | `security-engineer` |

**Done when:** you can copy a 4 GB file from S3 to local with constant memory and
a working cancel; browse three levels of nested archive; and a crafted archive
containing `../../.ssh/authorized_keys` is refused with a clear message.

**Acceptance gates:**
- Fuzz targets on every archive header parser (`cargo-fuzz`), running in CI.
- Integration tests against real backends via `testcontainers` (SFTP, S3/MinIO, FTP).
- `security-engineer` verdict of SAFE TO MERGE on the extraction path. Non-negotiable.

---

### M4 — Windows, tabs and fluidity

**The gap this closes:** two fixed panels. After M4 it is a workspace.

Norton Commander itself never had more than two panels — Far Manager and Total
Commander added tabs, and nothing in that lineage had real windows. So this is
the point where the plan stops inheriting and starts deciding.

| Work | Owner |
|---|---|
| Floating window manager: z-order, focus stack, keyboard+mouse move/resize, snap | `tui-engineer` |
| Tiling: N panels, splits, per-panel tabs, saved layouts | `tui-engineer` |
| **Open / close a window**, from the keyboard and from a clickable `×` in its title bar | `tui-engineer` + `ux-keeper` |
| **Move a window left / right** with `Ctrl+←` / `Ctrl+→`; grow/shrink with `Ctrl+Shift+←/→` | `tui-engineer` |
| Migrate M1's modal dialogs onto the real window manager | `tui-engineer` |
| Image preview (`ratatui-image`: kitty/iTerm2/sixel, half-block fallback) | `tui-engineer` |
| Web browse pane: HTML → readable text, images inline | `tui-engineer` |
| **Navigation fluidity pass**: prefetch on cursor move, warm metadata cache, instant back/forward | `perf-engineer` |

**A key collision to settle before this milestone, not during it.**

`Ctrl-X` was proposed for closing a window. It is also the near-universal binding
for *cut*, which M2's cross-session clipboard needs for files — and a file
manager where `Ctrl-X` sometimes cuts a file and sometimes closes a window is a
file manager that will eventually close a window when the user meant to cut.

Recommendation: **`Ctrl-W` closes a window** (what every browser and editor
uses), `Ctrl-F4` as the MDI-convention alternative, and `Ctrl-X` stays *cut*.
`ux-keeper` owns the final call; whatever it decides, both are rebindable and the
title bar `×` is always there for the mouse.

**Done when:** four panels with tabs, a floating viewer and a chat window coexist,
all keyboard-drivable; a window can be opened, moved left and right, and closed
without touching the mouse — and equally without touching the keyboard; moving
the cursor down a directory tree feels instant because the next listing is
already warm.

**Acceptance gate:** keypress-to-frame p99 < 16ms with six windows open on a
loaded machine, measured, not asserted.

---

## Parallel tracks

These do not block the critical path and can run alongside it once their
prerequisite lands.

### Track S — Search (starts after M1)

1. **Fuzzy jump** (`nucleo`) — < 10ms across a million paths. Immediately useful, tiny.
2. **Content search** with the ripgrep crates linked, streaming results, Esc cancels.
3. **Incremental index** (`tantivy` + `notify`) — opt-in per tree, budgeted, pauses on battery.
4. **Vector index** (`fastembed` local, `hnsw_rs`, tree-sitter chunking) — nothing
   leaves the machine by default; the user can see and purge everything indexed.

Owner: `search-engineer`. Every claim ships with a `criterion` benchmark on a
stated corpus.

### Track A — Agents (starts after M2; needs sessions to attach to)

1. **MCP client** (`rmcp`) over stdio, SSE and streamable HTTP — attached servers'
   tools become file-manager commands.
2. **LLM providers** behind one trait: Anthropic, OpenAI, Ollama. Streaming,
   cancellable, cost always visible.
3. **Chat window** in the floating WM, with the panel selection as context.
4. **MCP server**: expose the VFS and search to external agents. Deny by default,
   rooted at granted directories, read-only until the user grants writes.
5. **AI actions**: explain this file, bulk rename by intent, summarize a directory.

Owner: `ai-integration-engineer`, with `security-engineer` reviewing every step.
The hard rule: file contents, filenames, web pages and tool results are **data**,
structurally wrapped as untrusted — never instructions. Track S item 4 feeds this.

### Track P — Plank and effects

- [x] **Screensaver engine** — done in M0.5. Five effects, zero idle cost, the
      waking keypress swallowed by default so it cannot also confirm a delete.
- [ ] **More effects**: aquarium, boids, fire, tunnel, clock. One file plus two
      lines in the registry each — the extension cost was the point of the design.
- [ ] **TUI dock** *(after M4 — this one genuinely needs the window manager)*:
      auto-hide, magnification, launchers, running jobs, hosted sessions. Fully
      keyboard-drivable; a dock you can only use with a mouse fails in a file manager.
- [ ] **Transitions** via `tachyonfx`.
- [ ] **GPU backend** (`--features gpu`): the same `Canvas` in a `wgpu` window,
      with real shaders for the effects that want them. Effects declare which
      backends they support; the effect list is never forked per backend.

Owner: `fx-engineer`.

### Track X — Extensibility (starts after M3; needs a stable VFS API to expose)

WASM plugins (`wasmtime` + WIT, so plugins can be Rust/Go/Python/JS) and Lua
scripting (`mlua`). Both capability-gated, both with fuel limits and memory caps.
The WIT interface is versioned from day one — breaking it breaks users' plugins.

Owner: `plugin-engineer`.

### Track R — Shipping (starts now, continuously)

CI matrix from the first commit: macOS arm64/x86_64, Linux gnu/musl, Windows
MSVC. `cargo-deny` and `cargo-audit` blocking merges. `cargo hack
--feature-powerset` so `--features gpu` never silently breaks. Then signing,
notarization, and `cargo-dist` installers for Homebrew, WinGet/Scoop, deb/rpm/AUR.

Owner: `release-engineer`. **Start this before M1, not after M4** — a Windows
build that first runs at month four will not be a small fix.

---

## Definition of Done

Applies to every item above. An item is not done when the code compiles.

1. **Tests exist and a bug is reproduced before it is fixed.** Filesystem tests
   own a `TempDir`; one that touches `$HOME` or the repo is a defect.
2. **Clippy is warning-free** and `unwrap`/`expect` stay out of non-test code.
3. **All three platforms pass in CI.** "Works on my Mac" is not passing.
4. **The user can find it**: F-key bar or menu, command palette, and help.
5. **Errors say what happened, what was affected, and what to do about it.**
6. **Performance budgets hold** (see `CLAUDE.md`). A regression is a bug, not a tradeoff.
7. **Documentation lands with the feature**, not after.

---

## Risk register

Ranked by expected cost, not by likelihood alone.

| Risk | Why it hurts | Mitigation |
|---|---|---|
| **Data loss in file operations** | One incident and the project is dead. No amount of features recovers from it | Verify-before-delete; property tests; `security-engineer` and `qa-engineer` both review every change to `fileops` |
| **Zip-slip / path traversal** | The most likely way this app gets a CVE | `VfsPath::join` is the single chokepoint and already rejects `..`, separators and NUL. Fuzz every archive parser. Mandatory review before M3 merges |
| **Prompt injection through file content** | A README can tell a model with tool access to delete a repo | Content is structurally wrapped as data; destructive tools are opt-in per session and always confirmable |
| **Scope creep on the GPU backend** | `wgpu` + text shaping is a project of its own and could eat months | It stays behind `--features gpu` and is scheduled *last*, after the TUI is complete. The `Effect` trait keeps both backends sharing one implementation |
| **Windows arriving late** | Long paths, locked files, reserved names and the console API are not a "small fix" | Windows in CI from the first commit (Track R starts now) |
| **`ratatui` 0.30 API churn** | We are on a recent major; a breaking release mid-build costs a week | `Cargo.lock` is committed; upgrades are a deliberate task with a changelog read, never a drive-by `cargo update` |
| **Binary size from `wasmtime` + `tantivy` + `wgpu`** | The < 20 MB budget is genuinely tight | All three behind feature flags; `cargo-bloat` in CI with a size gate |
| **Agent-owned crates drifting apart** | 14 specialists can produce 14 dialects | The layering in `CLAUDE.md` is checked with `tokensave_dsm`/`tokensave_circular`; `rust-architect` reviews every cross-crate API |

---

## What happens next

The next action is **M1, starting with `dmac-config`** — every other item is
waiting on it to stop hardcoding paths and keys. In parallel, **Track R sets up
CI**, because the cost of adding Windows later grows every week.

Three things settled along the way, all recorded as ADRs: the core owns the state
and every UI is a renderer (ADR 0001), and hosting happens through a PTY rather
than by embedding other programs' windows (ADR 0002) — window reparenting is
unavailable on macOS and on Wayland, so it was never a cross-platform option —
and external GUI applications are driven by *switching* their windows rather
than embedding them (ADR 0003), which is available on macOS, Windows and X11 and
is what actually delivers "many VS Code windows, switchable from the session
list".

One caveat worth stating plainly: M2's hosting work is the most technically
uncertain thing in this plan. Terminal emulation is a deep well — a child program
that uses the kitty keyboard protocol, bracketed paste, mouse reporting and
synchronized output has to keep working through a layer that re-renders it, and
"mostly works" is very visible to the user. Budget accordingly, and prove it
against `claude` and a full-screen editor early rather than against `bash`.
