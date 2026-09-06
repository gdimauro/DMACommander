# DMACommander as an MCP server

An agent running *inside* DMACommander can see what DMACommander sees. That is
the whole idea: a coding agent that has to be told, in prose, where you are
every time you move is an agent working from a blurred photograph of your
screen.

## How it is wired

```
  claude ──stdio──> dmac --mcp <socket> ──unix socket──> the running commander
```

DMACommander listens on `<config>/mcp/<pid>.sock` for as long as it runs. When
it starts a shell for a session it writes a shim in
`<config>/shims/run-<pid>/<session>/` carrying the MCP description inline —
`--mcp-config` takes JSON as readily as a filename — so:

```sh
claude              # becomes: claude --mcp-config '{"mcpServers":{...}}' --resume <uuid>
```

Nothing to install and nothing to configure. `dmac --mcp <socket>` is a few
dozen lines of pipe: MCP clients start their servers themselves, as child
processes with pipes, and this is what puts one of them in touch with the
commander already on screen rather than a fresh one.

Nothing describing the server is written to a file. A file has to live
somewhere, and wherever that somewhere is a second commander wants it too:
session ids are indices, so two commanders each have a session `0`. When they
shared one, the second to start rewrote the first one's configuration to name
*its* socket, and every agent the first commander hosted was left pointed at a
socket that died with the other commander — configured, and answering nothing.
The description belongs to the run, so it travels inside the run's own shim.

The shim directory carries the pid for the same reason, and directories
belonging to runs that have ended are swept at startup.

The one thing that does *not* move with the run is the marker recording that a
conversation has been started once: it lives in `<config>/agents/`, because it
describes the conversation rather than the run and has to outlive both.

The environment carries the same facts for anything that is not `claude`:

| Variable            | What it is                          |
| ------------------- | ----------------------------------- |
| `DMAC_MCP_SOCKET`   | The socket to connect to            |
| `DMAC_SESSION`      | The session's name                  |
| `DMAC_CONVERSATION` | The agent conversation it owns      |

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
