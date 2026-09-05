# Architecture

## The shape of the thing

DMACommander is an orthodox file manager — two panels, one active, F-keys along
the bottom — wrapped around a virtual filesystem and a set of subsystems that no
1986 file manager had: a window manager, an agent runtime, a search index, a
plugin host and a GPU renderer.

The single structural idea that makes all of it work: **the core owns the state,
everything above it is a renderer.** The TUI draws a snapshot of `dmac-core` and
sends back intents. The GPU backend draws the same snapshot. A future headless
RPC mode would too. No subsystem above the core is allowed to hold state the core
cannot reconstruct.

## Crate map

| Crate | Responsibility | Owner agent |
|---|---|---|
| `dmac-config` | config, keymaps, themes; platform paths | `rust-architect` |
| `dmac-core` | entries, panels, jobs, the file-operation engine | `fileops-engineer` |
| `dmac-vfs` | `VfsBackend` trait + every provider | `vfs-engineer` |
| `dmac-session` | named sessions, workspace persistence, agent reattach | `session-engineer` |
| `dmac-search` | fuzzy, full-text and vector search | `search-engineer` |
| `dmac-view` | viewer, editor, hex, diff, image preview | `tui-engineer` |
| `dmac-fx` | dock, screensavers, transitions | `fx-engineer` |
| `dmac-tui` | ratatui UI, window manager, input routing | `tui-engineer` |
| `dmac-gpu` | optional wgpu window rendering the same frames | `fx-engineer` |
| `dmac-agent` | MCP client + server, LLM providers, agent loop | `ai-integration-engineer` |
| `dmac-plugin` | WASM (wasmtime/WIT) and Lua (mlua) host | `plugin-engineer` |
| `dmac` | argument parsing, session resolution, startup | `release-engineer` |

## Dependency choices

Chosen for maintenance, licence (MIT/Apache-2.0/BSD/MPL only) and cross-platform
support on all three targets. Versions are pinned in `Cargo.lock`.

### In use today

| Crate | Why this one |
|---|---|
| `ratatui` 0.30 | the TUI standard; immediate-mode, testable via `TestBackend` |
| `crossterm` | the only backend that is genuinely good on Windows |
| `tokio` | multi-threaded runtime; directory walks and network VFS run while the UI draws |
| `unicode-width` / `unicode-segmentation` | a filename with an emoji must not tear the panel border |
| `jiff` | modern, correct timezone handling without `chrono`'s baggage |
| `thiserror` | typed errors at every crate boundary |
| `clap` | derive-based CLI |

### Planned, by subsystem

**VFS** — `opendal` is the backbone: one API for S3, GCS, Azure Blob, WebDAV,
HTTP, FTP, SFTP, Dropbox, Google Drive and OneDrive, maintained by Apache. Beats
hand-rolling ten clients. Plus `russh`/`russh-sftp` for SSH cases OpenDAL cannot
reach (agent auth, jump hosts), `zip`/`tar`/`flate2`/`zstd`/`sevenz-rust2` for
archives browsed as directories, `keyring` for credentials, `moka` for metadata
caching.

**File operations** — `jwalk` for parallel traversal, `trash` for recoverable
delete, `reflink-copy` for instant CoW clones on APFS/Btrfs/XFS/ReFS, `blake3`
for verify-while-copying, `xattr`/`filetime` for metadata fidelity.

**Search** — `nucleo` for fuzzy matching (the matcher behind Helix), the actual
ripgrep crates (`grep-searcher`, `grep-regex`, `ignore`) linked rather than
shelled out to, `tantivy` for the persistent full-text index, `notify` for
incremental invalidation. The vector layer uses `fastembed` (ONNX, local, no API
key) with `hnsw_rs` for ANN and `tree-sitter` + `text-splitter` for
structure-aware chunking — so semantic search works with nothing leaving the machine.

**Agents** — `rmcp`, the official Rust MCP SDK, for both the client and the
server we expose. `reqwest` + `eventsource-stream` for provider HTTP, `schemars`
for tool schemas.

**Effects and GPU** — `tachyonfx` for shader-like post-processing inside the
terminal, `wgpu` + `winit` + `cosmic-text` for the optional native window,
`palette` for colour interpolation that does not look muddy, `ratatui-image` for
kitty/iTerm2/sixel image preview.

**Plugins** — `wasmtime` with the Component Model and WIT interfaces, so plugins
can be written in Rust, Go, Python or JS; `mlua` for quick scripting. Both
sandboxed with explicit capability grants and hard resource limits.

**Persistence** — `rusqlite` (bundled) for session state and index metadata,
`directories` for platform-correct config/cache/data paths.

## Concurrency model

One Tokio multi-threaded runtime. The UI event loop runs on the main thread and
does exactly three things: drain input, drain state updates, draw.

Input arrives from a dedicated OS thread doing a blocking `event::read()` and
forwarded over a channel — cheaper and more portable than an async event stream,
and it means the loop redraws when something happens rather than on a timer.

Directory listings stream. `VfsBackend::list` sends `ListChunk`s over a *bounded*
channel: if the UI falls behind, the walk applies backpressure instead of
buffering a million entries into memory. Each panel carries a generation counter,
so results from a directory the user already left are dropped rather than painted.

## The trust boundary

This program has an unusually wide attack surface for a file manager. Three rules
that are structural, not advisory:

1. **`VfsPath::join` rejects `..`, separators and NUL.** It is the single
   chokepoint that stops zip-slip and traversal from a hostile archive or a
   malicious server listing. Everything that descends into a child goes through it.
2. **File contents, filenames, web pages and MCP tool results are data.** They
   are wrapped and labelled untrusted before reaching a model with tool access.
3. **Deny by default at every boundary** — plugin capabilities, MCP roots, LLM
   tool access, network hosts.

Details and the ranked threat model: `.claude/agents/security-engineer.md`.
