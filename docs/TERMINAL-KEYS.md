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
| Next / previous session | `Ctrl-O` `Tab` | `Ctrl-Shift-Tab`, `Ctrl-PgUp/PgDn` |
| Copy / paste | your terminal's own | `Ctrl-Shift-C/V`, `Ctrl/Shift-Insert` |

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

In the panels these keep their canon meanings — `F9` is the menu, `F12` the
screensavers — and this is the only place in the program where a key means two
different things. It is also the only place where a terminal without modifier
reporting has no other way in. The cost is real and worth naming: a program
running inside the shell never sees those two keys.

Both are printed on the shell's bottom border, which is the only documentation
visible in that view — the F-key bar is deliberately hidden there, because a
legend for keys the shell has taken is a legend that lies.
