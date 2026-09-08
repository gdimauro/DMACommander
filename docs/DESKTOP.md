# Opening an editor, and putting it next to the commander

`Ctrl-H` opens the directory history. `F5` on any row opens that directory in
your editor and puts the two windows side by side: four fifths of the screen to
the editor, one fifth to the terminal running DMACommander.

It works on both kinds of row. A directory row opens that directory; a session
row opens wherever that session currently is.

## Which editor

`code`, unless `DMAC_EDITOR` says otherwise — a name to look up on `PATH`, or an
absolute path taken as given.

It is invoked as `code <dir>`. A folder that is already open in some window is
brought forward rather than loaded again — that is deliberately the editor's
decision and not ours, since it already knows which folders it has open and
asking it is more reliable than matching on window titles. A folder that is not
open gets a window of its own, which is what lets several be reopened at once.

## Which screen

The one the terminal is already on, not the main one. With several monitors the
answer is the monitor you are looking at, so the screen is chosen by finding
which one contains the terminal window's centre.

The arithmetic crosses two coordinate systems. Cocoa measures screens from the
bottom left of the main screen, with y growing upward; the accessibility API
measures windows from its top left, with y growing downward. Screen rectangles
are converted once, on the way in — `y_ax = main_height - (y_cocoa + height)` —
and everything after that is ordinary.

`visibleFrame` rather than `frame`, so the menu bar and the Dock keep their
space instead of being covered.

## Coming back to a different set of monitors

A session remembers where its editor window was, and the next start puts it
back. The first version of that remembered one rectangle —
which turned out to be a rectangle in the coordinate space of one particular
arrangement of monitors. `x = -3412` means "the display to the left" only while
there is one. Come back with the laptop alone and the window is restored
entirely off-screen, where it reads as lost.

So a window's place is remembered **per arrangement of monitors**, and the
terminal's too:

- The office — an ultrawide beside the laptop — is one arrangement, and the
  window has its place there.
- Home, the laptop alone, is another, with its own place.
- Come back to the office and you get the office's place back, exactly. Not a
  projection of home's; the one you left.

An arrangement is named by the set of displays' **full** frames, sorted and
written out — `-3440,0,3440,1440|0,0,1512,982` is recognisably "the ultrawide
and the laptop", and a session file stays readable by a person. The full frame
and not the usable one, deliberately: the usable one moves with the menu bar
and the Dock, and a Dock that grew six pixels must not turn the office into a
new place. The full frame changes only when displays are rearranged or a
resolution is chosen, and both of those *are* a different arrangement.

**An arrangement never seen before** has no "where I left it". The window is
then mapped onto the screen you are on — the one holding this terminal — by
proportion rather than by pixels: the right two-thirds of the ultrawide becomes
the right two-thirds of the laptop, and nothing is ever left hanging off an
edge where its title bar cannot be reached.

The terminal window comes back only in an arrangement it has been seen in. On a
new one it stays where you just opened it: you put it there on purpose, and
moving it would be rearranging your desk for no reason.

## Which windows come back is yours to say

Nothing is reopened silently. At startup, the same dialog that offers to resume
last time's agents also lists every editor window the last run had open — one
row per session, every row ticked — and waits. Untick the ones you do not want
this morning and press `y`; the rest come back, each at its place for this set
of monitors. `n` reopens nothing.

Saying no forgets nothing: the session still remembers where its window was,
entering the session still finds it if it is open, and the next start offers it
again. The list is only offered at all if you left the "reopen the same windows"
box ticked on the way out — untick that at `F10` and the next start opens the
panels and nothing else.

Closing the editor at home forgets home's place and nothing else. The office's
is still a fact about the office.

### Written once, on the way out

Where the windows are is written at exactly one moment: `F10`, with "remember
their positions" ticked. Every session's window is asked about then — not only
the visible session's — and what each is found to be, at that moment, under
the arrangement of monitors of that moment, is what the next start puts back.
The terminal itself goes under the same key. A window not open at that moment
loses this arrangement's entry and no other.

Nothing is written while the program runs: not when you enter a session, not
when you leave one, not when a window is docked from `F9`. That used to be
otherwise — the window was read on every entry and exit of a session, and the
final write only re-read the session on screen — and it meant two wrong
things at once. A window moved while you were looking at another session came
back where it had been when its session was last left, not where you put it;
and unticking "remember" on the way out could not take back what switching
sessions had already written an hour earlier. What you see as you leave is
what you get when you come back, and the tick is the whole of the decision.

The window asked about is the one the session recorded, on the folder it was
opened for — not wherever its panel has wandered since. A session with no
record is asked about the folder its panel is on, so a window you opened by
hand is found and written down the first time you leave with it open.

Files written before any of this carried one rectangle and no screen. They are
still read; that rectangle becomes the fallback for arrangements that have no
place of their own.

## Docking, on request

`F9` then `r` puts the editor on the right of the screen and this terminal on
the left; `F9` then `l` the other way round. The editor is the one for the
active panel's directory — opened if it has no window yet, brought forward by
the editor itself if it has — and the terminal takes whatever the editor left
of the row, exactly adjacent to it, on the screen the terminal is on.

The same key again narrows the editor a step: four fifths of the screen, then
two thirds, then half, then four fifths again. That is the one habit borrowed
from Windows, where a snap key pressed twice walks a window through its sizes.
Switching sides keeps the share, so `r` after `l` is only a swap.

Nothing docks by itself. Entering a session raises a window; the startup dialog
puts windows back where they were; docking happens when you ask for it and not
otherwise, because a window that moves on its own is a window you have to go
and find. Dock, then leave with `F10` and "remember the positions" ticked, and
the docked layout is what this arrangement of monitors remembers.

Which window moves is decided the way it is everywhere here: the editor is
asked to open the folder, and the window it brings to the front is the one
that is placed. A window title is read only so the placement can stop waiting
the moment that window is at the front, never to choose one — a wrong guess
about what to raise brings the wrong project forward, which is visible and
undone by looking away; a wrong guess about what to move rearranges your desk.

## The permission

Moving another application's windows needs **Accessibility**, and macOS refuses
until it is granted:

```
osascript is not allowed assistive access.
```

Grant it to your *terminal*, not to `dmac`: a command-line program sending Apple
Events is attributed to the application responsible for it, which is the
terminal it was started from.

**System Settings → Privacy & Security → Accessibility**, then add Terminal,
iTerm, Ghostty — whichever you start DMACommander in.

Without it the editor still opens. Only the placement is skipped, and the status
line says so rather than reporting a failure: opening the folder was the thing
that was asked for, and it happened.
