# Roadmap

The list of *what* gets built, with the agent that owns each item.

For the order it happens in, the acceptance criteria and the risks, see
**[PLAN.md](PLAN.md)** — that is the document to read first.

## Done

- [x] Cargo workspace, 12 crates, layering enforced by review — `rust-architect`
- [x] `Entry`/`Panel` core: natural-order sort, viewport windowing, orthodox
      selection semantics (`..` unselectable, cursor fallback) — `fileops-engineer`
- [x] `VfsBackend` trait with a capability matrix; streaming chunked listings;
      `VfsPath::join` as the traversal chokepoint — `vfs-engineer`
- [x] Local backend: symlinks as links, bounded reads, non-UTF-8 names survive,
      abandoned walks stop quietly — `vfs-engineer`
- [x] TUI: two panels, F-key bar, live command line, Norton keymap, theme,
      panic-safe terminal guard — `tui-engineer`
- [x] Golden-frame tests via `TestBackend`; 39 tests; clippy clean — `qa-engineer`

## Next — the foundation everything else sits on

- [ ] **`dmac-config`**: platform paths (`directories`), TOML config, keymap
      presets (`norton`/`far`/`mc`/`total`/`vim`), themes — `rust-architect` + `ux-keeper`
- [ ] **Session engine**: named sessions, `--session` / `$DMAC_SESSION` /
      `.dmac-session` / picker resolution, atomic debounced workspace snapshots,
      crash recovery — `session-engineer`
- [ ] **Claude session reattach**: record the Claude Code sessions a workspace had
      open and *resume* them on reopen, never start fresh; degrade loudly when a
      session is gone — `session-engineer`
- [ ] **File operation engine**: copy/move/delete with two-phase progress,
      conflict dialogs, resume, verify-before-delete, trash, CoW clones — `fileops-engineer`
- [ ] **Viewer (F3) and editor (F4)**: syntax highlighting, hex mode, image
      preview via kitty/iTerm2/sixel — `tui-engineer`

## Browse windows

- [ ] **Tiling panels + tabs**: N panels, splits, per-panel tabs, saved layouts — `tui-engineer`
- [ ] **Floating window manager**: real z-order, focus stack, keyboard and mouse
      move/resize, snap, maximize — `tui-engineer`
- [ ] **Remote VFS**: OpenDAL backends (S3/GCS/Azure/WebDAV/FTP/Drive/Dropbox),
      SFTP via russh, archives browsed as directories, nested — `vfs-engineer`
- [ ] **Web browse pane**: HTML → readable text in a panel, images inline — `tui-engineer`
- [ ] **Navigation fluidity**: prefetch on cursor move, warm metadata cache,
      instant back/forward, zero-latency directory switch — `perf-engineer`

## Search

- [ ] **Fuzzy jump** (`nucleo`) across a million paths in <10ms — `search-engineer`
- [ ] **Content search** with the ripgrep crates, streaming results — `search-engineer`
- [ ] **Incremental index** (`tantivy` + `notify`), budgeted and opt-in — `search-engineer`
- [ ] **Vector index** (`fastembed` local by default, `hnsw_rs`, tree-sitter
      chunking) so an LLM can answer questions over your files — `search-engineer`

## Agents

- [ ] **MCP client** (`rmcp`): attach servers over stdio/SSE/streamable HTTP;
      their tools become file-manager commands — `ai-integration-engineer`
- [ ] **MCP server**: expose the VFS and search to external agents, sandboxed,
      read-only until granted — `ai-integration-engineer` + `security-engineer`
- [ ] **LLM providers**: Anthropic, OpenAI, Ollama behind one trait; streaming,
      cancellable, with cost always visible — `ai-integration-engineer`
- [ ] **Chat window + AI actions**: explain a file, bulk rename by intent,
      summarize a directory — `ai-integration-engineer`

## Plank and effects

- [ ] **TUI dock**: auto-hide, magnification, launchers, running jobs and
      attached agent sessions, fully keyboard-drivable — `fx-engineer`
- [ ] **Screensaver engine**: matrix, pipes, aquarium, starfield, plasma, life;
      idle-triggered, zero CPU when hidden, any key dismisses — `fx-engineer`
- [ ] **Transitions** via `tachyonfx` — `fx-engineer`
- [ ] **GPU backend** (`--features gpu`): the same frames in a wgpu window, with
      real shaders for the effects that want them — `fx-engineer`

## Extensibility and shipping

- [ ] **WASM plugins** (wasmtime + WIT) and **Lua scripting** (mlua), both
      capability-gated with hard resource limits — `plugin-engineer`
- [ ] **Command palette** (Ctrl-Shift-P) exposing every command with its
      binding — `ux-keeper`
- [ ] **Undo stack** for file operations, with visible history — `fileops-engineer`
- [ ] **CI matrix** (macOS arm64/x86_64, Linux gnu/musl, Windows MSVC),
      `cargo-deny`, signing and notarization, `cargo-dist` installers — `release-engineer`
