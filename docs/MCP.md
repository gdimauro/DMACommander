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
it starts a shell for a session it writes an MCP configuration next to that
session's shims and points the agent at it, so:

```sh
claude              # becomes: claude --mcp-config .../shims/<id>/mcp.json --resume <uuid>
```

Nothing to install and nothing to configure. `dmac --mcp <socket>` is a few
dozen lines of pipe: MCP clients start their servers themselves, as child
processes with pipes, and this is what puts one of them in touch with the
commander already on screen rather than a fresh one.

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
