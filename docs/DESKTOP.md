# Opening an editor, and putting it next to the commander

`Ctrl-H` opens the directory history. `F5` on any row opens that directory in
your editor and puts the two windows side by side: four fifths of the screen to
the editor, one fifth to the terminal running DMACommander.

It works on both kinds of row. A directory row opens that directory; a session
row opens wherever that session currently is.

## Which editor

`code`, unless `DMAC_EDITOR` says otherwise — a name to look up on `PATH`, or an
absolute path taken as given.

It is invoked as `code -r <dir>`, which reuses the last active window rather than
adding a fifth one. If a window already has that folder open, VS Code brings it
forward instead of loading it again. That is deliberately the editor's decision
and not ours: it already knows which folders it has open, and asking it is more
reliable than matching on window titles.

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
