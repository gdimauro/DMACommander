# DMACommander as an MCP server

An agent running *inside* DMACommander can see what DMACommander sees. That is
the whole idea: a coding agent that has to be told, in prose, where you are
every time you move is an agent working from a blurred photograph of your
screen.

## How it is wired

```
  claude ──stdio──> dmac --mcp <socket> ──unix socket──> the running commander
```

DMACommander listens on `<config>/mcp/<pid>.sock` for as long as it runs, and
tells every shell it hosts where that is. `dmac --mcp <socket>` is a few dozen
lines of pipe: MCP clients start their servers themselves, as child processes
with pipes, and this is what puts one of them in touch with the commander
already on screen rather than a fresh one.

Named by pid, so two commanders on one machine never fight over it. Sockets
left by runs that have ended are swept at startup.

| Variable            | What it is                          |
| ------------------- | ----------------------------------- |
| `DMAC_MCP_CONFIG`   | This commander, as `--mcp-config` takes it |
| `DMAC_MCP_SOCKET`   | The socket to connect to            |
| `DMAC_SESSION`      | The session's name                  |
| `DMAC_SESSION_ID`   | The session, as `--mcp-session` takes it |
| `DMAC_CONVERSATION` | The agent conversation it owns      |

## Connecting an agent to it

Nothing on `PATH` is touched, so this is something you ask for — one line, in a
shell DMACommander is hosting:

```sh
claude --session-id "$DMAC_CONVERSATION" --mcp-config "$DMAC_MCP_CONFIG"
```

`--mcp-config` last, and not as a matter of taste: it takes *several* values, so
it swallows every following word that does not begin with a dash. Put it earlier
and a plain argument — a directory, a prompt — is read as another configuration
file, and the agent refuses to start over a path nobody wrote.

That is exactly what the utilities menu types for you (`F9`, then `c`) — with
`--resume` in place of `--session-id` once that conversation exists, so closing
the commander and coming back reopens the conversation instead of starting a
fresh one. There is no flag meaning "either": `--session-id` is refused for a
conversation that already exists and `--resume` for one that does not, so the
choice is made by looking in the agent's own store
(`$CLAUDE_CONFIG_DIR`, or `~/.claude`). A marker file of ours was the
alternative, and it was wrong exactly when it mattered — the conversation
outlives our bookkeeping.

Both are the *variable* and not its value on purpose: the value is JSON full of braces
wrapped around a socket path with a space in it, and a command line carrying
that is one nobody can read — and one that `zsh` would try to glob before
`claude` ever saw it. Through the environment it arrives as a single argument,
already correct.

Worth an alias in your own rc file if you start agents by hand. The description
names this session, so a tool call is answered about the panels the agent is
running inside rather than whichever session happens to be on screen.

There was once a shim for this — a `claude` script planted early on the hosted
shell's `PATH` that added all of the above by itself. It is gone. `PATH` is not
ours to keep: the shell reads its rc files after we hand over, and the ordinary
`export PATH="$HOME/.local/bin:$PATH"` puts that directory in front of ours. So
the shim worked or did not depending on someone else's dotfiles, and when it did
not, it failed silently — the agent started perfectly well, knowing nothing. A
line you wrote yourself is worth more than a mechanism that is right most of the
time and gives no sign when it is not.

## The tools

| Tool             | What it does                                                        |
| ---------------- | ------------------------------------------------------------------- |
| `state`          | Both panels, their directories, cursor and selection; which is active |
| `list`           | Entries of a directory — the active panel's by default               |
| `navigate`       | Take a panel somewhere. The user sees it move                        |
| `select`         | Set, add to or clear a panel's selection by name                     |
| `command`        | Put a line on the command line, and optionally run it                |
| `history`        | Directories visited, across sessions and restarts                    |
| `sessions`       | Every open session                                                   |
| `switch_session` | Bring one to the screen                                              |
| `notify`         | A line in the status bar                                             |
| `screen`         | The commander's screen as text, rendered on demand                   |

Reading files is deliberately absent: every agent already has that, and a
second, subtly different implementation of it is a liability.

## The rule these follow

**Read freely, write visibly.** Anything that changes what the user is looking
at says so in its result and shows on screen. `command` defaults to *typing* a
line rather than running it — it is the user's shell, in the user's directory,
and they press Enter. An agent that quietly moved someone's panels would be a
poltergeist.

Calls are answered on the UI thread, between frames, with the whole application
in hand: no locks, and no chance of reporting a panel that moved between the
read and the reply. A call is answered about the session the *calling agent* is
hosted in, not about whichever session happens to be on screen.
