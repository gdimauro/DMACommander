# Guided tours — the help that shows you

`F1` opens the help. At the top of it is the list of tours: click one, or
press `t` for them as a menu and a letter to pick. The program then
demonstrates that part of itself, on the real screen, with a caption saying
what is about to happen and the keys lit up as they are pressed.

```
╭ Tour · Marking files · 3/10 ──────────────────────╮
│ Space marks it — and moves on, so a run of files  │
│ is one key held down.                             │
│                                                   │
│  Space                                            │
╰──────────────────────────────────── Esc stops ────╯
```

Nothing is faked. A tour drives the program the way a person would — the same
keys through the same input path, the same clicks through the same hit-tests —
and the panels move because the keys moved them. If a key stopped working, the
tour would visibly stop working with it. That is the point of driving the real
thing, and it is why every tour is also a test that replays end to end.

## What a tour will not do

**Touch your files.** Every tour runs in a session of its own, on a scratch
tree the tour creates in the temp directory — a README, a few files, a `src/`,
a `photos/`, an `out/` to copy into — and removes when it ends. That session is
never written to disk: a crash mid-tour does not bring back a folder that is
gone. Stop a tour with Esc halfway through and the tree is still
removed: the cleanup is on drop, not on a step. A demonstration of F8 that
deleted something of yours would be a demonstration of why people do not trust
demonstrations.

**Launch anything behind your back.** A step that would open your editor, start
an agent, or trust a project's commands is *shown* — the key cap lights, the
caption explains what it would do — and not pressed. Those are your decisions,
and the tour says so.

**Quit.** The tour of the quit dialog opens it and leaves with Esc. A test
checks that no tour ends inside that dialog, and that no tour presses the keys
that start things.

## Watching one

- The caption says what is *about* to happen, before the key is pressed. You
  read it, then you see it.
- The key caps light as the key goes down: `Ctrl` `+` `T`, `F5`, `Shift` `+`
  `↓`. Typed text fills in a character at a time, the way a person types.
- For a click, a cross lands on the thing that is about to be clicked, drawn on
  exactly the cell the program's own hit-test would answer for.
- **Esc stops the tour** at once. Every other key is held back while one plays,
  so a stray keystroke cannot wander into the demonstration.

## The tours

| | |
|---|---|
| The panels | two panels, and how to move between and inside them |
| Marking files | Space and Shift, and what the footer says about it |
| Copying and moving | F5 and F6, and why the other panel is already filled in |
| Deleting, carefully | F8 asks, and goes to the trash |
| Making a folder | F7, and a name several levels deep |
| Looking at a file | F3, the viewer: text, hex, and finding things |
| Sessions | Ctrl-T, typing to find one, and groups |
| The shell | Ctrl-O, and the four keys that still reach the commander from inside |
| Your own commands | F2, and why a project's commands do not run until you say so |
| Agents and the editor | F9: an agent that can see the panels, and the editor beside them |
| Leaving, and coming back | F10 asks two things; the next start asks one |

## Adding one

A tour is data: `crates/dmac-tui/src/tour.rs`, `SCENARIOS`. A scenario is a
name, a line about it, and a list of steps — `say` a caption, `key` a press,
`type_in` some text, `click` a target, `show` a key without pressing it. Add
one there, and a row for it in the help's first section (a test holds the two
lists together), and it plays, and is replayed by the tests. The scratch tree
lists as `..`, `out`, `photos`, `src`, `README.md`, `recipe.txt`, `todo.md`;
a step that says "Down, twice" counts on that.

Two tests will refuse a tour that is unsafe: one that ends inside the quit
dialog, or one that presses a key that launches something. Write the caption in
the present tense, about what is *about* to happen — a caption describing what
just happened is read after the screen already changed, which is too late to
watch for it.
