# Changelog

Notable changes, newest first. Versions follow the policy in
[docs/VERSIONING.md](docs/VERSIONING.md).

## Unreleased

### Added
- Startup splash showing version, build number, commit, build time, rustc and
  target. Drawn over the panels rather than instead of them, so the app never
  looks like it is still loading when it is already usable. Any key dismisses it,
  and that key is swallowed. `--no-splash` skips it.
- Build identity captured at compile time (`dmac-config::build_info`), surfaced
  by `--version`, `--build-info` and the splash. The commit count serves as a
  monotonic build number, and a dirty working tree is marked so a SHA is never
  misleading.
- `docs/VERSIONING.md` and this changelog.

### Fixed
- **Startup took 2.0 seconds** in any terminal without the kitty keyboard
  protocol. `supports_keyboard_enhancement()` queries the terminal and waits for
  a reply that such terminals never send, burning the full 2s timeout in exactly
  the case where the answer is "no". The flags are now pushed without asking —
  an ordinary CSI sequence, discarded by terminals that do not implement it.
  First frame went from 2004ms to 4ms, against an 80ms budget. Set
  `DMAC_NO_KEYBOARD_ENHANCEMENT=1` to opt out.

## 0.1.0 — 2026-09-05

Initial commit. Two panels with streaming listings, the Norton keymap, an
explicit focus model, full mouse support, five screensavers plus Snake, a VFS
trait with a local backend, and panic-safe terminal restore.
