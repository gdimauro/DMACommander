# ADR 0001: The core owns the state; every UI is a renderer

**Status:** accepted
**Date:** 2026-09-05

## Context

DMACommander must render in three places that do not share a drawing model: any
terminal (ratatui), an optional GPU window (wgpu), and — eventually — headlessly,
so an external agent can drive it over MCP. The obvious approach, letting each
frontend hold its own panel state, forks the logic three ways and guarantees they
drift.

There is a second force: the app must stay at 60fps while copying 100k files,
walking a million-entry directory, or streaming tokens from a model. Any design
where the UI owns state tends to put work on the thread that draws.

## Decision

`dmac-core` holds all application state. Frontends render a snapshot and emit
intents; they store nothing the core cannot reconstruct.

Concretely, a panel exposes its scroll window rather than its contents:

```rust
impl Panel {
    /// Told by the renderer how many rows fit.
    pub fn set_viewport(&mut self, rows: usize);
    /// The rows to draw, and the index of the first one. O(rows on screen).
    pub fn visible(&self) -> (usize, &[Entry]);
}
```

Work that can block goes to a Tokio task and returns as a message:

```rust
enum Update {
    Entries { panel: PanelId, generation: u64, chunk: ListChunk },
    Error   { panel: PanelId, message: String },
}
```

The `generation` counter is what makes navigation safe: it is bumped on every
directory change, and updates carrying a stale generation are dropped rather than
painted into the wrong directory.

## Consequences

Easy: a second backend renders identical frames with no logic duplicated; the UI
is testable headlessly with `TestBackend` (this is how the panel width bugs were
caught before a human saw them); a million-entry panel costs the renderer the
same as a ten-entry one.

Hard: the frontend cannot take shortcuts by stashing derived state — every piece
of it has to earn a place in the core. Cross-crate borrows need care; `ui::draw`
destructures `App` because the panels are borrowed mutably (they record their
viewport height) while the theme is read.

Wrong if: we ever need per-frontend state that genuinely has no meaning in the
core. Cursor blink phase is the likely first case; it will live in the frontend
as an explicit, documented exception.

## Alternatives rejected

- **Frontend-owned state, core as a library of helpers** — forks the logic per
  backend and makes headless mode impossible without a third implementation.
- **A retained-mode widget tree shared by both backends** — would mean writing a
  cross-backend layout engine, which is a project of its own and buys nothing that
  immediate-mode rendering from a snapshot does not already give us.
