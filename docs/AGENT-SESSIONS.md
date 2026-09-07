# Agents, sessions, and the things that must stay true

A DMACommander session can host a coding agent. Closing the commander and
opening it again has to put that agent back **in the conversation it was in**,
and never in somebody else's.

This page is not a description of how that works — the code says that, and the
code changes. It is the list of properties that must hold **however** it works,
why each one is there, and what it looked like when one of them did not.

Read it before changing anything in `crates/dmac-session/src/agent.rs` or in
`crates/dmac-tui/src/mcp.rs`. Both have already been broken by good reasoning.

---

## The invariants

### 1. A resumed agent lands in a conversation that belongs to its session

Never one chosen by a default. Every command that starts an agent either names
the session's own conversation, or carries a rule whose answer is *local* —
`-c` continues the most recent conversation **in this directory**, a fork makes
a new one. Both of those produce an answer belonging to this session.

Enforced by `a_resumed_agent_never_lands_in_a_conversation_chosen_by_a_default`,
which asserts the property over the whole space of saved command lines rather
than over a list of examples.

### 2. Nothing comes back carrying a selector with no value

`--resume`, `-r` and `--session-id` must always be followed by a value. A bare
one is a picker, and it is also what an id that went missing looks like by the
time it reaches a shell.

### 3. The agent is the authority on which conversation it is in

If a running agent's command line names a conversation, **that** is the
session's conversation from then on — not the one the commander remembered.
Type `claude --resume <something-else>` in a hosted shell and the session
follows you there.

`--session-id` outranks `--resume` when both are present, because a fork is
`--resume <mother> --fork-session --session-id <new>` and the conversation it
*becomes* is the third one. Reading those the other way round resumes every
sibling into its mother.

### 4. A fork happens once

`--fork-session` belongs to a session's birth. It is dropped when the session
comes back, because replaying it would branch a new conversation on every
restart — each from the same mother, and none of them yesterday's.

### 5. An id travels as a variable, never on the command line

The line says `--resume "$DMAC_CONVERSATION"`, and the hosted shell's
environment holds the value. Two reasons: the line stays something a person can
read and correct before pressing Enter, and thirty-six characters never have to
survive being quoted through a shell. The same goes for `--mcp-config`, which is
JSON containing a path with a space in it.

Whatever the line spends, the environment must set. They read the same field,
and a line naming one conversation while the variable holds another is a
mismatch nothing on screen would show.

### 6. A tool that acts on a session refuses when that session is gone

An agent is given its session's id at startup. If the session has since been
closed, every tool that acts on one must refuse — never fall back to whichever
session is on screen.

Which tools those are is **data**, in `crates/dmac-mcp/src/tools.rs` as
`Scope::Session`, and
`every_session_scoped_tool_refuses_an_agent_whose_session_is_gone` enumerates
the catalogue rather than keeping its own list. A tool added later that forgets
the check fails that test by name.

`Scope::Reading` tools may still answer about the session on screen: a bridge
that named no session at all is a person running the tools by hand, and they
mean "here".

---

## What it looked like when these were not true

Both of these were found in a real session, on a running instance, in one
afternoon. They are here because an invariant with a story attached is one
people believe.

### The picker that put eight sessions in the wrong conversation

Invariant 1, violated. A bare `--resume` was replayed exactly as the user had
typed it, justified as handing them back their own choice.

`ps` on the running instance:

```
23532 claude --resume --mcp-config {"mcpServers":{"dmac":{...,"--mcp-session","1"}}}
23038 claude --resume --mcp-config {"mcpServers":{"dmac":{...,"--mcp-session","2"}}}
```

No id, on two sessions. A restart reattaching nine sessions puts nine pickers on
screen; the obvious thing to do with each is press Enter; the picker's default
is the most recent conversation **anywhere**. One session belongs there. The
other eight are in somebody else's — and the one that had been busiest all day
was the commander's own, so that is where they went.

Worse, it is silent and it perpetuates itself. The running process still shows
only `claude --resume`, so nothing outside can ever learn where that session
went. Its stored conversation stays a value unrelated to what is on screen, and
the next restart does it again.

**How the regression got in**, which is the part worth remembering: the old test
defended the *mechanism* — "a bare `--resume` silently opens whichever
conversation was most recent". Checked against `claude --help`, that description
turned out to be inaccurate: it opens an interactive picker. The description was
wrong and the behaviour it was protecting was right, and overturning one took
the other with it.

That is why invariant 1 is written as a consequence and tested as a property. A
consequence does not depend on how any CLI words its help.

### The orphaned agent that moved somebody else's panels

Invariant 6, violated. `mcp_index` fell back to the current session when the id
an agent had been given did not resolve. An agent whose session had been closed
therefore received the user's workspace, and its next `cd` moved panels that had
nothing to do with it — with nothing on screen to say why, and the wrong
directory written to the session file at shutdown.

The visible symptom was a set of sessions whose names and directories had
drifted apart: a session called `CAEP.Modeler` sitting in
`SemolificioLoiudice.AI/docs`, and nine of eleven right-hand panels showing `/`.

An error is recoverable. Quietly acting on the wrong workspace is not.

---

## Two things that cannot be known from outside, and are not guessed

**Which conversation a picker landed on.** The command line does not say, and
the transcript is not held open, so the agent's file descriptors do not say
either. This was checked, not assumed.

**Which transcript belongs to which agent, by recency.** Matching the newest
file in the project directory is wrong in exactly the case this program is built
for: several commanders open on one repository, each with its own agent, all
appending into the same directory. A guess that is usually right is not good
enough for something whose failure is "you are in someone else's conversation".

Where an answer cannot be known, the code says so rather than picking one.

---

## A second agent

`plank` is a peer of `claude` and names conversations differently — a
leading-slash `/resume`, sha-prefix ids, and no way to dictate an id at all.
Every invariant above still applies; what changes is that the *shape* of an id
becomes a property of the program rather than a constant. See item 5 of
`docs/BACKLOG.md`.
