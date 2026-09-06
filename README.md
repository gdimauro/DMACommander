# DMACommander

An orthodox file manager for people who never stopped missing Norton Commander,
built for 2026.

Two panels, F-keys along the bottom, the command line always live — and behind
that familiar surface: a virtual filesystem that makes an S3 bucket or a tar
inside a zip look like a folder, floating windows, attachable MCP servers and LLM
agents, a plugin host, named sessions that restore everything you left open, a
dock, and screensavers.

Rust. macOS, Windows, Linux. Any terminal, with an optional GPU window.

## Status

Early. The skeleton runs: two panels listing real directories with streaming
loads, Norton keybindings, natural-order sorting, selection with the orthodox
cursor fallback, and a terminal guard that restores your shell even on a panic.

```sh
cargo run -- . ~
```

`Tab` switches panels · arrows and PgUp/PgDn navigate · `Enter` enters a
directory · `Backspace` goes up · `Ins` selects · `Alt-U` swaps panels ·
`Ctrl-U` the utilities · `Ctrl-H` everywhere you have been · `Ctrl-O` hides the
panels · `Shift-F3..F6` sort · `F10` quits.

A directory tree can name the session it belongs to: put the name in a
`.dmac-session` file and `cd` into it from anywhere — `dmac` picks it up, after
`--session` and `$DMAC_SESSION` and before anything else.

In a hosted shell, `Shift-PgUp/PgDn` reads back through what it has printed and
`Shift`+arrows selects it, over as many screenfuls as you like; `Ctrl-T` opens
the session rail, whose two widths you can drag or resize with `Left`/`Right`.
See [docs/TERMINAL-KEYS.md](docs/TERMINAL-KEYS.md).

DMACommander is an MCP server for as long as it runs, so an agent hosted in a
shell here can see both panels, the history, the sessions and the screen it is
running inside. `F9` then `c` starts `claude` already connected to it — by
typing the line at the shell, where you can read it before it runs:

```sh
claude --mcp-config "$DMAC_MCP_CONFIG"
```

Every hosted shell is given that variable, so the same line works by hand.
See [docs/MCP.md](docs/MCP.md).

Nothing is put on your `PATH` and no rc file of yours is written. An earlier
version planted a `claude` shim to do all of this by itself, which worked
exactly as long as no dotfile put its own directory first — and gave no sign
when one did.

Keys bound to subsystems that do not exist yet say so in the status line rather
than doing nothing.

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
- [docs/DESKTOP.md](docs/DESKTOP.md) — opening an editor beside the commander,
  and the one permission it needs.
- [docs/adr/](docs/adr/) — why the non-obvious decisions were made.

## Licence

MIT OR Apache-2.0.
