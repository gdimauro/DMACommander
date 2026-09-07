# Which keys your terminal can actually send

Some of DMACommander's keys work everywhere and some depend on the terminal you
opened it in. That is not a preference and not a bug: a terminal emulator sends
*bytes*, and most of the ones in use cannot express "Ctrl and Shift and H" as
anything distinguishable from "Ctrl and H".

This page says which keys are safe, which are not, and what to press instead.

## The short version

| You want | Everywhere | Also, if your terminal can | 
| --- | --- | --- |
| Leave the shell | `Ctrl-O` | — |
| Directory history | `Ctrl-O` `h`, or `F12` | `Ctrl-Shift-H`, `Cmd-H` |
| Utilities | `Ctrl-O` `u`, or `F9` | `Ctrl-Shift-U`, `Cmd-U` |
| Open this directory in the editor | `F9` then `o` | — |
| Next / previous session | `Ctrl-O` `Tab` | `Ctrl-Shift-Tab`, `Ctrl-PgUp/PgDn` |
| Go to a session by name | `F9`, then its digit | `Ctrl-T`, `Alt`+digit |
| The help | `F1`; from a shell, `Ctrl-O` `F1` | — |
| A screensaver, right now | `F12` `F12` | `Shift-F12` |
| The next screensaver, while one shows | `F12` | `Shift-F12` |
| Copy / paste | your terminal's own | `Ctrl-Shift-C/V`, `Ctrl/Shift-Insert` |
| Read back through a shell | `Shift-PgUp/PgDn` | `Ctrl-Shift-Up/Down/Home/End` |
| Select text in a shell | `Shift` + arrows | — |

From the **panels** everything works, because there a plain `Ctrl` is enough:
`Ctrl-H` is the history and `Ctrl-U` the utilities. The problem is only inside a
hosted shell, where those bare keys belong to the program you are running.

## Why

`Ctrl-H` is byte `0x08`. So is `Backspace`. So is `Ctrl-Shift-H`, in every
terminal that does not implement a protocol for reporting modifiers separately —
and the plain-`0x08` ones cannot tell you which of the three you pressed, because
the information never left the keyboard driver.

DMACommander asks for the **Kitty keyboard protocol** at startup, which does
carry the modifiers (`CSI 104;6u` is Ctrl-Shift-H, unambiguously). Terminals that
answer it get the full keymap. Terminals that ignore it get a keymap with holes,
and there is nothing the application can do about that from its side.

| Terminal | Reports modifiers | Notes |
| --- | --- | --- |
| kitty, Ghostty, WezTerm | yes | everything works |
| iTerm2 | yes | everything works |
| foot, Alacritty (recent) | yes | everything works |
| Apple Terminal | **no** | use `Ctrl-O` chords or `F9`/`F12` |

On macOS there is a second trap: `Cmd-H` is *Hide Application*, handled by the
system before any terminal sees it. Where `Cmd` bindings are listed they only
apply to terminals configured to forward the Command key, which is not the
default anywhere.

## The two ways in

### `Ctrl-O` and one letter

`Ctrl-O` leaves the shell, as it always has. For exactly one keypress afterwards
it also answers a chord:

```
Ctrl-O          leave the shell            (unchanged)
Ctrl-O  h       leave, and open the history
Ctrl-O  u       leave, and open the utilities
Ctrl-O  Tab     leave, and go to the next session
Ctrl-O  F1      leave, and open the help
```

Anything else you type is an ordinary keypress, so the chord costs nothing but
those letters, and only in the instant after leaving a shell. It needs no
modifier reporting whatsoever, which is why it works over `ssh`, inside `tmux`,
and in terminals that implement nothing at all.

### `F9` and `F12`, from inside a shell only

Inside a hosted shell every F-key normally goes to the child. Two do not:

```
F9      utilities
F12     directory history
```

`F9` means the same thing in the panels: the utilities. It used to be Norton's
pull-down menu, which was never written and answered "not implemented yet" — a
key that apologises is worse than a key that does the useful thing, and one key
with one meaning everywhere is worth more than fidelity to a menu whose
contents live on their own keys here anyway.

`F12` is the one key in the program that means two different things: the
screensaver picker in the panels, the directory history in a shell. It is also
the only way in for a terminal that cannot report modifiers. The cost is real
and worth naming: a program running inside the shell never sees either key.

Both are printed on the shell's bottom border, which is the only documentation
visible in that view — the F-key bar is deliberately hidden there, because a
legend for keys the shell has taken is a legend that lies.

## Reading back through a shell, and selecting what is up there

A hosted shell keeps 2000 lines above the top of the pane. The rule is one line
long:

> **Ctrl-Shift looks. Shift selects.**

```
Shift-PgUp / PgDn        a page back, and forward again
Ctrl-Shift-Up / Down     a line at a time
Ctrl-Shift-PgUp / PgDn   a page, even while something is selected
Ctrl-Shift-Home / End    the oldest line held, and back to the live screen
wheel                    three lines a notch

Shift + arrows           select, from where the shell's cursor is
Shift + Home / End       to the start or the end of the line
Shift + PgUp / PgDn      extend the selection by a page
Ctrl-Shift-C             copy it
Esc                      back to the live screen, selection cleared
```

Holding `Shift-Up` at the top of the pane keeps going: the view scrolls and the
selection grows with it, so a selection may be many screenfuls tall, and copying
it gives you all of it — not the part that happened to be on screen.

Typing anything else snaps back to the live screen, the way every terminal does.
`Esc` only means "back to live" while there is something to come back from;
otherwise it reaches the hosted program, which needs it.

Of these, only the `Ctrl-Shift` family needs modifier reporting. `Shift-PgUp`,
`Shift` with the arrows and the wheel work in every terminal, and between them
they cover everything — the `Ctrl-Shift` keys are quicker, not necessary.

## Resizing the session rail

The strip of coloured dots down the left edge has two widths, remembered
separately and kept across restarts: one for when it is resting and one for when
it is open.

```
Ctrl-T  or  Shift-Tab    open the rail (it takes the keyboard)
  Left / Right, - / +    make it narrower or wider
drag its right-hand edge  the same, with the pointer, open or not
```

`Shift-Tab` opens it **from the panels only**. Inside a shell that key belongs to
whatever is hosted — it is how `claude` cycles its permission modes — so it goes
to the child and the rail stays on `Ctrl-T`. `Ctrl-Shift-Tab` still moves between
sessions from in there, but only in a terminal that reports modifiers: without
the kitty protocol it arrives as the same three bytes as `Shift-Tab`, and nothing
on this side can tell the two apart.

Widen the **resting** strip past 8 columns and it stops being dots: it shows the
session names and paths all the time, without being opened. Drag it down to
nothing and it disappears entirely — `Ctrl-T` still opens it.

Both widths can also be set on the command line, which is the way to undo a drag
that went too far:

```sh
dmac --rail-collapsed-width 20 --rail-width 30
```
