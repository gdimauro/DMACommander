# F2 — your own commands

F2 opens a list of commands you wrote, each on a letter. Press the letter, the
command runs in this session's shell. Norton Commander had exactly this, and it
is most of why people kept using it: the twenty commands you actually run in a
project, one keystroke away, without leaving the panels.

It is also **the only key in this program that runs a command you wrote**, which
is why half of this page is about where a command is allowed to come from.

## Two files

| File | Whose | Runs? |
| --- | --- | --- |
| `~/.config/dmac/menu.toml` | yours | always |
| `./.dmac-menu.toml` | the directory's | **only once you trust it** |

The first is read from `~/.config/dmac/` when that directory exists — even on
macOS, so that a dotfiles repository holds all of DMACommander's configuration
or none of it. Otherwise it goes to the platform's own location.

The second is looked for in the directory the active panel is showing. It is
what makes F2 worth having: `build`, `test`, `deploy` and `logs` are different
in every project, and a menu that cannot say so is a menu you stop opening.

## Trust, and why there is any

A per-directory menu, taken naively, is remote code execution by `cd`. Clone a
repository to read it, press F2 out of habit, and you have run whatever its
author put in that file. Far Manager works that way and it is a known way to be
caught out.

So a directory's menu is **shown and not runnable** until you say otherwise:

```
┌──────────────────────────────────┐
│  b  build            (blocked)   │
│  t  test             (blocked)   │
│  d  deploy           (blocked)   │
│                                  │
│  this directory carries 3        │
│  command(s) — T to trust it   T  │
└──────────────────────────────────┘
```

You can read every command it offers. None of them has a letter that answers,
and pressing one does nothing — an accelerator printed next to something that
will not answer is a lie about what the key does.

`T` approves it. What is recorded is a **hash of the file's contents**, not its
path, in `~/.config/dmac/trusted-menus.toml`. That distinction is the whole
point: approving a path once would mean running whatever arrives in it after the
next `git pull`, written by somebody else. A menu that has changed since you
approved it is a new question, and it goes back to being inert until you answer
it again.

The same file at a different path is also a different question, because the
thing you approved was "this file, here".

## The format

```toml
[[entry]]
key   = "b"
title = "cargo build"
run   = "cargo build"

[[entry]]
key     = "c"
title   = "clean target"
run     = "rm -rf target"
confirm = true

[[entry]]
key   = "d"
title = "diff the marked files against main"
run   = "git diff main -- {paths}"
```

| Field | | |
| --- | --- | --- |
| `key` | required | one character; the letter that runs it |
| `title` | required | what the menu shows |
| `run` | required | the command, as a template |
| `confirm` | optional | type it and wait for Enter instead of running it |

Two entries on the same letter is an **error**, not a silent first-wins: a menu
with two `b`s has one you cannot reach, and finding out which by experiment is
not a thing to make someone do.

`confirm` is per entry because the answer is per entry. `cargo build` wants one
keystroke. `rm -rf` wants to be read first — with the substitutions already
made, which is exactly the moment a surprising filename shows itself.

## Placeholders

| | |
| --- | --- |
| `{name}` | the file under the cursor |
| `{path}` | its full path |
| `{stem}` | its name without the extension |
| `{ext}` | its extension, without the dot |
| `{names}` | every marked file, or the one under the cursor |
| `{paths}` | the same, as full paths |
| `{dir}` | this panel's directory |
| `{other}` | the other panel's directory |

`{names}` and `{paths}` follow the same rule as every other operation in the
program: the files you have marked, or — when you have marked none — the one the
cursor is on.

`{{` and `}}` are a literal `{` and `}`, so `awk '{{print $1}}'` works. A menu
that could not express that is one people would work around.

An unknown placeholder is an error and the command does not run. Leaving
`{nmae}` to reach the shell would run something *almost* right, where the typo
arrives as a literal brace and becomes a filename, a glob, or nothing at all
depending on the shell's mood.

A placeholder with nothing to fill it — `{name}` with an empty panel — is also
an error. A command whose operands vanished is a command that would run on
whatever is left, and "whatever is left" is how `rm -rf {paths}` becomes
`rm -rf`.

### Every value is quoted, and there is no way to ask for it not to be

`{name}` for a file called `my notes.txt` becomes `'my notes.txt'` — one
argument. For a file called `; rm -rf ~` it becomes `'; rm -rf ~'`, which is a
filename and not four instructions.

Those are legal names. They arrive from archives, from downloads, from other
people's repositories, and from anyone who has ever pasted into a rename box.
Unquoted, a filename stops being data and becomes an instruction, which is
rule 5 of this codebase applied to the most ordinary content there is.

There is deliberately **no raw form**. An escape hatch would be used — somebody
would want to pass several flags, reach for a `{path:raw}`, and have it work all
afternoon until it met a filename with a space in it. If a command needs shell
syntax it writes the shell syntax itself: the *template* is yours and is not
quoted, and only the values put into it are.

```toml
# Fine — the template is yours, the pipeline is real shell.
run = "grep -n TODO {paths} | sort -u"

# Also fine — $HOME is expanded by the shell, not by us.
run = "cp {paths} $HOME/backup/"
```

## Where the command runs

In this session's hosted shell, written in as if you had typed it — not spawned
out of sight. Three reasons, all the same reason:

- you see what ran, exactly as it was after substitution;
- the output goes where output goes, and stays in the scrollback;
- it inherits the shell's environment and its working directory, so a command
  that depends on your `PATH` or a virtualenv behaves the way it does when you
  type it yourself.

The shell has to be at a prompt. A line typed at something already running is
not a command — it is a sentence handed to whatever has the keyboard, and if
that is an agent, it will answer it.

## Keys

```
F2              open the menu
↑ ↓             move
Enter           run the highlighted command
<letter>        run that command directly
T               trust this directory's menu
Esc  ·  F2      close
```

## Where it lives in the code

- `crates/dmac-config/src/menu.rs` — the format, the trust store, and
  `expand`, which is the function that does the quoting.
- `crates/dmac-config/src/lib.rs` — `shell_quote`. It lives at the bottom of
  the stack so that `expand` cannot be written without it.
- `crates/dmac-tui/src/app.rs` — reading both menus, the trust prompt, and
  putting the expanded line into the shell.

The security-critical behaviour has tests that name what they are protecting
against: `a_hostile_filename_stays_an_argument`,
`editing_a_trusted_menu_asks_again`, and
`a_directory_menu_is_visible_and_inert_until_it_is_trusted`.
