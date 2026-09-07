# Backlog

The working queue. `docs/PLAN.md` carries the phase order and the acceptance
gates; this file is what is actually owed, in the order it is being taken.

Each item says what "done" means, because an item whose finish line is a matter
of opinion is an item that never closes.

---

## 1. An editor window comes back where it was — **done**

A session that had `code` open on its folder must, on re-entry, get it back **at
the same coordinates**: same screen, same position, same size.

- Read the window's frame while it is open (System Events, by title — reading is
  safe; a wrong guess records the wrong numbers, it does not move anybody's
  window).
- Persist it on the session, beside the conversation.
- On coming back to that session, reopen the folder and place *the window that
  just appeared* — never one matched by title, which is the rule
  `dmac-desktop::window_for` already states: title matching decides what to
  **raise**, never what to **move**.

**Done when:** open `code` from a session, move and resize its window, quit
DMACommander, start it again, return to that session — the window is where you
left it. And when the editor is already open on that folder, nothing moves.

**Where it landed.** `dmac_desktop::editor_frame_for` reads it,
`restore_editor` puts it back, `Session::editor` keeps it, and the round trip
through the session file is tested. Reading is proven against this machine —
with five projects open at once, the frame comes back for the project's own
directory and `None` for every other, parents and sub-crates included
(`cargo test -p dmac-desktop -- --ignored --nocapture what_this_machine`).
Restoring is *not* proven live, deliberately: the only way to prove it is to
move the user's real windows, and that is not something to do unasked.

## 2. The help arrives on a 3D spiral — **done**

A screensaver that flies the characters of the help page in along an unwinding
helix, settles them into the page, holds ~10s, flies them out the same way and
comes back with the next screenful.

- Lives in `dmac-fx` as an effect; takes its words from `Effect::set_text`,
  which already exists and is already fed `help::plain_lines()`.
- Each loop shows the next page of the help, so the hold is long enough to
  actually read one.
- **Second half, once the effect works:** the same fly-in becomes how `F1` opens
  the help — but then it stays until `Esc` rather than cycling.

**Done when:** it is in the picker, it dismisses on any key like every other
screensaver, and the text it settles into is legible at 80×24.

**Where it landed.** `crates/dmac-fx/src/effects/helix.rs`, in the picker as
`helix`. Both halves are in: the screensaver cycles pages, and `F1` now opens
the help through `Helix::entrance()` — the same arrival, run once, settling into
the real scrollable page. A key during the arrival lands the page *and* still
does what it says, because a key that only cancels an animation is a key that
did not do what it was pressed for.

## 3. Every function key does something — **done**

Thirteen keys still answer "not implemented yet", which is worse than a key that
does nothing: it is a key that promises.

- `F3` view, `F4` edit, `F5` copy, `F6` move/rename, `F7` make directory,
  `F8` delete.
- `F5`/`F6`/`F8` are `fileops-engineer`'s: bytes actually move, so they go
  through the job engine in `dmac-core` with progress, conflict resolution and
  verification before deleting — never a shell-out.

**Done when:** no user-facing key answers "not implemented yet", and every
destructive one is covered by a test that proves it verifies before it deletes.

**Where it stands.**

- `F2` **done** — the user menu, with a trust model. See `docs/USER-MENU.md`.
- `F3` **done** — the viewer, in `dmac-view`: text or hex, capped read, search.
- `F4` **done** — opens the file in the editor you already have.
- `F5` copy, `F6` move, `F7` make directory, `F8` delete — **done**, over the
  new engine in `crates/dmac-core/src/fileops/`. F5 and F6 ask where, with the
  other panel pre-filled; F8 asks first and goes to the trash; conflicts stop
  the job and ask, with Shift meaning "and every one after this".

**What the engine guarantees**, checked before it was wired: a move never
verifies less than a blake3 hash *whatever the options say*, because the step
after it deletes the original — `required_verify` forces it and a test pins it.
The verification re-reads the written file from disk rather than trusting the
hash taken in flight, a mismatch is an error rather than a warning, and the `?`
on the copy is what stands between a failed verify and `remove_source_file`.
Symlinks are not followed by default; `sources_from` treats joining a listing's
name to a directory as a security boundary, so a backend that names
`../../.ssh/authorized_keys` does not get to choose where we write.

**Still owed here:** a real progress dialog. Progress is currently one line in
the status bar, which is enough to see that a job is alive and not enough for a
long one.

**A known fragility, not caused by this:** the three pty tests that drive a real
shell and wait for its output (`shift_page_up_reads_back_through_the_shell`,
`shift_selection_over_the_shell_runs_past_one_screenful`,
`a_selection_survives_output_while_the_view_is_held_back`) fail occasionally
when the machine is busy compiling. They pass 3/3 alone and in an unloaded full
run. They race on output arriving rather than waiting for it, and that is what
should be fixed.

## 4. Settings, with the agent, the model and the editor in them

- **Agent**: which coding agent a session hosts. `claude` today; `plank` next
  — see item 6, which is where the real work is.
- **Model**: Claude by default, and selectable — OpenAI, and the local runners
  people actually use (Ollama, LM Studio, llama.cpp, anything speaking an
  OpenAI-compatible endpoint). Note that `plank` reaches most of those itself,
  through `--provider openai --base-url`, so for a plank session this is its
  setting to pass on rather than ours to implement.
- **Editor**: VS Code by default, and changeable. `DMAC_EDITOR` already does
  this on the command line; it needs to be a setting with a UI.
- Written through `dmac-config`, which is where configuration belongs.

## 5. Hosting `plank` beside `claude`

"plank" is [aovestdipaperino/plank](https://github.com/aovestdipaperino/plank) —
same author as `tokensave`, checked out at `~/prj/plank`, installed at
`~/.cargo/bin/plank`. It is a coding **agent**, a peer of `claude`: a Rust agent
with a Ratatui TUI, a REPL, a headless mode and its own tools. Not a provider,
which is where it would have gone if nobody had asked.

So it belongs in `ATTACHED` in `crates/dmac-session/src/agent.rs` — recognised
when it is running in a hosted shell, and put back on restart. That list was
built to take a second entry. What it was *not* built for is how differently
plank names a conversation, and each difference breaks something that is
currently true:

| | `claude` | `plank` |
|---|---|---|
| resume | `--resume <uuid>` | `/resume <prefix>` — a literal leading slash |
| id shape | a UUID | a sha prefix (`~/.plank/kvcache/<12-hex>/`) |
| dictating an id | `--session-id <uuid>` | **no equivalent** — resume only |
| MCP | inline JSON | `--mcp-config FILE`, over a global `~/.plank/.mcp.json` |

- `looks_like_conversation` demands 36 characters of UUID, so it will never
  adopt a plank session. The shape has to become a property of the program.
- The whole "mint an id up front and hand it to the agent" model does not
  transfer: plank's id is chosen by plank. What is left is observation — and a
  bare `/resume` takes the most recent, which is the hazard the current code
  already refuses to walk into.
- The global `~/.plank/.mcp.json` is the way to advertise this commander to it
  without writing per-project files, which is the thing we deliberately stopped
  doing for `claude`.

**Done when:** start `plank` in a hosted shell, quit DMACommander, start it
again — the session comes back in the same plank conversation, and picking
between the two agents is a setting rather than a recompile.

## 6. Untangling the two sessions — **done**

Not a feature, but it is owed.

- The session-group work (tree view in the rail, `F9 → a` for a session beside
  this one) is **finished and green in the working tree but not committed**: its
  F9 entry extends `SessionRow`, which belongs to another session's uncommitted
  work. Committing mine alone produced a broken tree three times because the two
  halves are genuinely entangled.
- ~~`shift_f12_starts_a_screensaver_and_then_walks_the_catalogue` is flaky~~ —
  **fixed.** It started at a random effect and `next` deliberately walks the
  games and demos too, which keep their keys to steer with; the test now walks
  on to a screensaver before asserting that any key dismisses. Eight consecutive
  passes, where it used to fail about one run in four.

**Done when:** the working tree is committed, and the suite is green twice in a
row on a cold build.

**How it went.** They did not come apart, so they went in together as one
commit that says which half is which. Verified on a cold build in a detached
worktree: 508 tests, no failures, clippy clean.
