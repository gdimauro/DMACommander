# ADR 0002: Host child programs through a PTY, never by embedding their windows

**Status:** accepted
**Date:** 2026-09-05

## Context

DMACommander must run other programs *inside itself*: a shell, `claude`, an
editor, a dock. The obvious model, borrowed from desktop environments, is window
embedding — take the other program's native window and reparent it into ours.
The question was whether the three target platforms support that.

They do not, and not equally:

| Platform | Cross-process window reparenting |
|---|---|
| **Windows** | `SetParent()` works across processes. Semi-supported, with real caveats around input focus, DPI and message loops |
| **Linux / X11** | `XReparentWindow` works; this is what XEmbed and the old Qt/GTK plugin containers were built on |
| **Linux / Wayland** | Deliberately impossible. The protocol has no notion of a client adopting another client's surface |
| **macOS** | No public API. `NSWindow.addChildWindow(_:ordered:)` relates only windows **your own process owns**. Apple's own out-of-process UI (Quick Look, share sheets) goes through private `NSRemoteView`/`NSRemoteViewService` SPI. The community workaround is sharing an `IOSurface`, which requires cooperation from the other program |

So the best case is three different implementations, one of which is private
API on macOS and one of which is simply unavailable on Wayland — for a feature
that is central to the product.

## Decision

**Hosting happens at the PTY layer, not the window layer.**

`dmac-pty` spawns the child on a pseudo-terminal it owns, runs the output
through a terminal emulator, and renders the resulting cell grid like any other
content. The child is genuinely ours: we own its stdin, its stdout, its size,
and its lifetime.

```rust
/// A hosted child. The same trait whether it is a shell, `claude`, or a REPL.
pub trait Hosted {
    fn write_input(&mut self, bytes: &[u8]) -> Result<()>;
    fn resize(&mut self, cols: u16, rows: u16) -> Result<()>;
    /// The emulated screen, renderable into a panel, a floating window, or fullscreen.
    fn screen(&self) -> &Screen;
    fn exit_status(&self) -> Option<ExitStatus>;
}
```

One implementation, three platforms: `portable-pty` covers Unix PTYs and Windows
ConPTY, and nothing in the design touches a window server.

## Consequences

Easy: identical behaviour everywhere, including over SSH, where window embedding
is meaningless and a PTY is native. The hosted screen is just a `Screen`, so it
renders in a panel, in a floating window, or fullscreen with no special cases —
the same property that lets the GPU backend render it. Sessions can persist and
restore a hosted program because we control its spawn.

Hard: this only hosts programs that speak terminal. A GUI application cannot be
embedded at all, on any platform, under this decision.

The concrete casualty is **Plank the dock**: it is a GTK application, so we
cannot host the real thing. The dock is therefore *implemented* in `dmac-fx`
rather than embedded — which was already the plan, and which is better anyway: a
dock that knows about our jobs, our sessions and our hosted processes is more
useful than a general-purpose one rendered in a box.

The other cost is fidelity. Terminal emulation is a deep well: a child using the
kitty keyboard protocol, bracketed paste, mouse reporting or synchronized output
has to keep working through a layer that re-renders it, and "mostly works" is
very visible. This is called out as the main technical risk in `docs/PLAN.md`,
and the mitigation is to test against `claude` and a full-screen editor early,
not against `bash`.

## Alternatives rejected

- **Per-platform window embedding** (`SetParent` / `XReparentWindow` / private
  `NSRemoteView`) — three implementations, one of them private API that App
  Review can reject and that Apple can break, and nothing at all on Wayland.
- **Screen capture plus input synthesis** (`ScreenCaptureKit`, the Accessibility
  API) — gives pixels without ownership: no reliable input routing, no resize
  control, requires intrusive permissions, and breaks entirely over SSH.
- **`IOSurface` sharing on macOS** — needs the *other* program to cooperate,
  which rules out every program we do not ship.

## Sources

- [Apple Developer Forums: embedding an NSWindow/NSView from another process](https://developer.apple.com/forums/thread/680330)
- [Apple Technical Note TN2213 (child windows)](https://developer.apple.com/library/archive/technotes/tn2213/_index.html)
