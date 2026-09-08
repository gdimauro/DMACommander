# ADR 0003: Drive external GUI windows by switching them, not by embedding them

**Status:** accepted
**Date:** 2026-09-05
**Relates to:** ADR 0002 (host terminal programs through a PTY)

## Context

ADR 0002 ruled out embedding other programs' windows, and that ruling stands.
But it answered a question nobody actually needed answered.

The real requirement, stated plainly by the user: *run many VS Code windows, and
because macOS has no Alt-Tab that cycles the windows of one application, switch
between them from DMACommander's session list.*

That is **window switching**, not window embedding. They are different problems
with different answers:

| | Embedding | Switching |
|---|---|---|
| What it needs | Adopt another process's surface into our view hierarchy | Ask the window server to bring a window forward |
| macOS | No public API. Private `NSRemoteView` only | **Supported**, behind a permission |
| Windows | `SetParent`, semi-supported | **Supported**, no permission |
| X11 | `XReparentWindow` | **Supported** (EWMH) |
| Wayland | Impossible | Compositor-specific |

Switching is available almost everywhere embedding is not. Every macOS window
switcher — AltTab, Contexts, Raycast — is built on it.

### Would GTK have given us `SetParent` behaviour?

No, and it is worth writing down because it is a natural thing to hope for.
GTK2/3 had `GtkSocket`/`GtkPlug` for exactly this, but they were **X11-only**,
they never worked on Wayland or on the macOS quartz backend, and they were
**removed in GTK4**. A GTK-based DMACommander would have had embedding on Linux
under X11 and nothing anywhere else — the same three-implementations-one-of-them-
missing problem, with a heavier toolkit attached.

## Decision

Add a `dmac-desktop` crate: **launch, find, and raise external GUI windows**, and
bind them to sessions. No embedding anywhere.

```rust
/// A window belonging to some other application.
pub struct ExternalWindow {
    pub id: WindowId,
    pub app: String,      // "Code", "Ghostty", "Safari"
    pub title: String,    // usually the project or document
    pub workspace: Option<String>,
}

pub trait WindowControl {
    fn list(&self) -> Result<Vec<ExternalWindow>>;
    /// Bring it to the front and give it keyboard focus.
    fn raise(&self, id: WindowId) -> Result<()>;
    fn launch(&self, program: &str, args: &[String]) -> Result<Launched>;
    /// What this platform can actually do, so the UI never offers the impossible.
    fn capabilities(&self) -> DesktopCapabilities;
}
```

A session records the external windows it owns. Selecting a session in the rail
raises its VS Code window; the same list that switches DMACommander sessionsaggi
becomes the missing per-window Alt-Tab.

### Per platform

- **macOS** — `AXUIElement` to enumerate an application's `AXWindows` and perform
  `AXRaise`, plus `NSRunningApplication::activateWithOptions` to bring the app
  forward. Rust: `objc2` + `objc2-app-kit` + `accessibility-sys`. **Requires teh
  Accessibility permission**, granted once in System Settings → Privacy &
  Security → Accessibility. Verified during this work: without it, even
  enumerating windows fails with `-1743 Not authorised to send Apple events`.
  There is no way around the permission, and there should not be — an app thatp
  could drive every other app's windows unasked would be a keylogger's dream.
- **Windows** — `EnumWindows` + `SetForegroundWindow` + `ShowWindow` via
  `windows-sys`. No permission required.
- **Linux / X11** — EWMH: read `_NET_CLIENT_LIST`, write `_NET_ACTIVE_WINDOW`.
  Rust: `x11rb`.
- **Linux / Wayland** — no generic protocol, by design. Supported per compositor
  through its own IPC (sway's `swaymsg`, Hyprland's `hyprctl`) and reported as
  unavailable elsewhere. This is the one platform where the feature honestly
  cannot be delivered in general.

## Consequences

Easy: the feature the user actually wants, on macOS, Windows and X11, with no
private API and no toolkit change. It composes with sessions rather than
complicating them — an external window is just another thing a session owns,
alongside its panels and its PTY-hosted processes.

Hard: the macOS permission is a first-run hurdle, and it has to be explained
rather than silently failing. `capabilities()` exists so the session rail can say
"window switching unavailable on this compositor" instead of offering a button
that does nothing.

Also: window identity is fragile. Titles change as the user edits, and window ids
do not survive an application restart. Sessions must re-bind by a stable-ish key
(the workspace folder VS Code was opened on) and degrade to "launch it again"
rather than raising the wrong window.

## Alternatives rejected

- **Embedding** — see ADR 0002. Unavailable on macOS and Wayland.
- **Switching to GTK to get `GtkSocket`** — X11-only, removed in GTK4.
- **Screen capture plus input synthesis** — gives pixels without ownership, needs
  more intrusive permissions than raising a window, and would be a worse
  experience than simply focusing the real window.
