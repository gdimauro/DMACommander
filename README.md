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
natural-order sorting, selection with the orthodox cursor fallback, and a
terminal guard that restores your shell even on a panic.

```sh
cargo run -- . ~
```

`Tab` switches panels · arrows and PgUp/PgDn navigate · `Enter` enters a
directory · `Backspace` goes up · `Ins` selects · `Alt-U` swaps panels ·
`Ctrl-U` the utilities · `Ctrl-H` everywhere you have been · `Ctrl-O` the
session's shell and back · `Shift-F3..F6` sort · `F10` quits.

**The function keys do what they say.** `F1` help, `F2` your own commands,
`F3` view, `F4` edit, `F5` copy, `F6` move, `F7` make directory, `F8` delete,
`F9` utilities.

**Files move carefully.** Copy, move, delete, rename and mkdir run on their own
thread and report progress back. A move never verifies less than a blake3 hash
*whatever the options say*, because the step after it deletes the original — and
the verification re-reads what was written from disk rather than trusting the
hash taken in flight. Symlinks are not followed unless you mean it. Deletion
goes to the trash. Conflicts stop and ask, naming the file, with `Shift` meaning
"and every one after this".

**Sessions come back.** Named workspaces with their own directories, shell,
command line and history, restored where you left them — including the editor
window, on the screen and at the size it had. Sessions can hang off one another
in a group you can fold away: `a` in the rail opens one *beside* this one, and
the agent it starts forks the conversation of the session it came from, so it
begins knowing what that one knows.

**Agents are hosted, not wrapped.** DMACommander is an MCP server for as long as
it runs, so a `claude` in a shell here can see both panels, the history, the
sessions and the screen it is running inside. `F9` then `c` starts one already
connected — by typing the line at the shell, where you can read it first:

```sh
claude --resume "$DMAC_CONVERSATION" --mcp-config "$DMAC_MCP_CONFIG"
```

It rejoins the same conversation every time rather than starting a fresh one,
and it is watched while it runs rather than only at shutdown — so a `kill -9`
does not cost you the ability to get back to it. What that guarantees, and what
it looked like the afternoon it did not, is
[docs/AGENT-SESSIONS.md](docs/AGENT-SESSIONS.md).

Nothing is put on your `PATH` and no rc file of yours is written. An earlier
version planted a `claude` shim to do all of this by itself, which worked
exactly as long as no dotfile put its own directory first — and gave no sign
when one did.

**`F2` runs commands you wrote**, from your own menu and from a `.dmac-menu.toml`
a project carries. A project's menu is shown and **inert until you trust it**,
against a hash of its contents, because otherwise cloning a repository and
pressing F2 out of habit runs whatever its author put there. Every value
substituted into a command is shell-quoted with no way to ask for it unquoted: a
file can legally be called `; rm -rf ~`.
See [docs/USER-MENU.md](docs/USER-MENU.md).

A directory tree can name the session it belongs to: put the name in a
`.dmac-session` file and `cd` into it from anywhere — `dmac` picks it up, after
`--session` and `$DMAC_SESSION` and before anything else.

In a hosted shell, `Shift-PgUp/PgDn` reads back through what it has printed and
`Shift`+arrows selects it, over as many screenfuls as you like; `Ctrl-T` opens
the session rail, whose two widths you can drag or resize.
See [docs/TERMINAL-KEYS.md](docs/TERMINAL-KEYS.md).

There are screensavers, and the help page arrives on a three-dimensional spiral
because somebody asked.

### Not yet

`Ctrl-Shift-P` (a command palette), `Ctrl-F` (fuzzy find) and `Ctrl-Shift-A`
(a chat window) say so in the status line rather than doing nothing — they are
three subsystems that do not exist yet. The virtual filesystem is designed and
not written, so everything above is local files for now.

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
