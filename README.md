# DMACommander

An orthodox file manager for people who never stopped missing Norton Commander,
built for 2026.

Two panels, F-keys along the bottom, the command line always live — and behind
that familiar surface: named sessions that restore everything you left open,
hosted LLM agents that come back in the conversation they were in, and a file
engine that verifies a copy before it deletes the original.

Where it is going, and not there yet: a virtual filesystem that makes an S3
bucket or a tar inside a zip look like a folder, floating windows, a plugin
host, and a dock. The README says which is which, because a list that mixes
them is a list you cannot use to decide anything.

Rust, in 15 crates, with 647 tests. macOS, Windows, Linux. Any terminal, with an
optional GPU window.

## What works

Two panels listing real directories with streaming loads, Norton keybindings,
natural-order sorting, and a terminal guard that restores your shell even on a
panic. Everything below is built, tested and in daily use; what is not is under
[Not yet](#not-yet).

```sh
cargo run -- . ~
```

### The panels

- `Tab` switches panels, arrows and page keys move, `Enter` enters, `Backspace`
  goes up. `Alt-U` swaps the two.
- **Marking**: `Ins` or `Space` marks a file and moves on, so a run of files is
  one key held down. `Shift` with the arrows, page keys and `Home`/`End` extends
  a selection. `Esc` clears it.
- **Quick search**: type a letter and the cursor jumps; keep typing to narrow.
- `Shift-F3..F6` sort by name, extension, date, size.
- **`Esc` is a ladder.** It undoes the most recent thing — a running copy, a
  search, the marks, the command line — and only when nothing is left goes back
  to the shell. It never throws away a half-typed command.

### The function keys do what they say

| | |
|---|---|
| `F1` | the help — which is also a list of things to press, see below |
| `F2` | your own commands, from a menu you write |
| `F3` | view a file, text or hex |
| `F4` | edit it, in the editor you already have |
| `F5` `F6` | copy and move, asking where — the other panel pre-filled |
| `F7` | make a directory, several levels deep if you type them |
| `F8` | delete, to the trash, asking first |
| `F9` | the utilities, and a way out |
| `F10` | quit — it asks, and lets you decide two things on the way out |

### Files move carefully

Copy, move, delete, rename and mkdir run on their own thread and report
progress back. A move never verifies less than a blake3 hash *whatever the
options say*, because the step after it deletes the original — and the
verification re-reads what was written from disk rather than trusting the hash
taken in flight. Symlinks are not followed unless you mean it. Deletion goes to
the trash. Conflicts stop and ask, naming the file, with `Shift` meaning "and
every one after this".

### The viewer

`F3` opens a file without ever loading all of it: there is an 8MB cap, and going
past it is said on the frame rather than in a status line that scrolls away. A
bad byte does not refuse the file. A NUL in the first kilobytes means it is not
text, and it opens as hex. `/` finds, `n` and `N` walk the matches and wrap, and
the match is highlighted inside the line — not the whole line, which would say
*that* it matched and hide *where*.

### Sessions

Named workspaces, each with its own directories, shell, command line and
history, restored where you left them. `Ctrl-T` opens the rail:

- **Type to find one** — no key to press first, the cursor goes to the closest
  match, `Enter` enters it. `Esc` clears the search; left alone ten seconds, it
  clears itself.
- `F2` renames, `Del` closes, `Ctrl-N` opens a new one here.
- **Groups.** `Ctrl-A` opens a session *beside* this one — same directories,
  its own shell, and an agent that starts by knowing what this one knows, because
  it forks the conversation. `Space` folds a group away. Two levels and no more.
- `Alt-1..9` jumps by the number shown in the rail. The list never reorders,
  so those numbers stay true while you read them.

A directory tree can name the session it belongs to: put the name in a
`.dmac-session` file and `cd` into it from anywhere.

### Windows come back where they were — on *this* set of monitors

A session remembers its editor window: which folder, and where it sat, **per
arrangement of monitors**. The office with the ultrawide is one arrangement;
home with the laptop alone is another; each keeps its own places, and coming
back to the office gets the office back exactly. An arrangement never seen maps
the window onto the screen you are on by proportion, and nothing is ever left
hanging off an edge. The terminal DMACommander runs in comes back too.

Nothing is reopened silently. At startup, one dialog lists everything the last
run left open — agents and windows, every row ticked. Untick what you do not
want this morning and press `y`. Saying no forgets nothing.

`F10` asks two things on the way out: reopen the same windows next time, and
remember their positions on this set of monitors. Both are pre-ticked the way
you last left them, so the common case is `F10`, `Enter`.
See [docs/DESKTOP.md](docs/DESKTOP.md).

### Agents are hosted, not wrapped

DMACommander is an MCP server for as long as it runs, so a `claude` in a shell
here can see both panels, the history, the sessions and the screen it is running
inside. `F9` then `c` starts one already connected — by typing the line at the
shell, where you can read it first:

```sh
claude --resume "$DMAC_CONVERSATION" --mcp-config "$DMAC_MCP_CONFIG"
```

It rejoins the same conversation every time, is watched while it runs rather
than only at shutdown — so a `kill -9` does not cost you the way back — and
never lands in a conversation chosen by a default. What that guarantees, and
what it looked like the afternoon it did not, is
[docs/AGENT-SESSIONS.md](docs/AGENT-SESSIONS.md).

Nothing is put on your `PATH` and no rc file of yours is written.

### `F2` runs commands you wrote

From your own menu, and from a `.dmac-menu.toml` a project carries. A project's
menu is shown and **inert until you trust it**, against a hash of its contents —
otherwise cloning a repository and pressing F2 out of habit runs whatever its
author put there. Every value substituted into a command is shell-quoted with no
way to ask for it unquoted: a file can legally be called `; rm -rf ~`.
See [docs/USER-MENU.md](docs/USER-MENU.md).

### Everything named on screen can be clicked

The pointer is the one input nothing intercepts — not a hosted program that has
taken the keyboard, not a terminal that cannot spell a modifier. So every place
a command is named is a place you can press it: the shell's bottom border, the
F-key bar, every row of the help, the menus. If a key will not reach this
program, open `F1` and click the row.
See [docs/TERMINAL-KEYS.md](docs/TERMINAL-KEYS.md).

### The shell

`Ctrl-O` switches between the panels and this session's shell, and back. Inside
it, `Shift-PgUp/PgDn` reads back through what it printed and `Shift`+arrows
selects, over as many screenfuls as you like. The bottom border names the four
keys that still reach the commander from inside — and each is clickable.

There are screensavers, and the help page arrives on a three-dimensional spiral
because somebody asked.

### Not yet

`Ctrl-Shift-P` (a command palette) and `Ctrl-Shift-A` (a chat window) say so in
the status line rather than doing nothing — they are two subsystems that do not
exist yet. `Ctrl-F` searches the sessions; over a directory tree it does not
yet. The virtual filesystem is designed and not written, so everything above is
local files for now.

## Building

```sh
cargo build --release          # no GPU dependency
cargo build --release -F gpu   # adds the wgpu window
cargo test --workspace
```

## Layout

`crates/` holds one crate per subsystem; `.claude/agents/` holds the specialist
agent that owns each one, with the rules it works under.

- **[docs/PLAN.md](docs/PLAN.md)** — phases, acceptance gates, risks. Start here.
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — the shape and the dependency choices.
- [docs/ROADMAP.md](docs/ROADMAP.md) — the item list.
- [docs/TERMINAL-KEYS.md](docs/TERMINAL-KEYS.md) — which keys your terminal can
  actually send, and what to press when it cannot send them.
- [docs/MCP.md](docs/MCP.md) — the commander as a tool an agent can call.
- **[docs/AGENT-SESSIONS.md](docs/AGENT-SESSIONS.md)** — the properties that keep
  a hosted agent in *its own* conversation across a restart, why each is there,
  and what it looked like when one of them was not. Read before touching
  `dmac-session/src/agent.rs` or `dmac-tui/src/mcp.rs`.
- [docs/USER-MENU.md](docs/USER-MENU.md) — F2, and why a directory's commands do
  not run until you say so.
- [docs/DESKTOP.md](docs/DESKTOP.md) — opening an editor beside the commander,
  and the one permission it needs.
- [docs/adr/](docs/adr/) — why the non-obvious decisions were made.

## Licence

MIT OR Apache-2.0.
