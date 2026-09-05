# Versioning

## What a version is made of

```
dmac 0.1.0
build     42 · 9f3c1a7 (main)
compiled  2026-09-05 17:15 UTC
rustc     1.96.0
target    aarch64-apple-darwin
```

| Part | Where it comes from | What it is for |
|---|---|---|
| `0.1.0` | `Cargo.toml`, `[workspace.package]` | The semantic version. Human-chosen, changed deliberately |
| `42` | `git rev-list --count HEAD` | The build number. Monotonic, identical for anyone with the same history, needs no state file |
| `9f3c1a7` | `git rev-parse --short HEAD` | Exactly which source this came from |
| `dirty` | `git status --porcelain` | Present when tracked files did not match `HEAD`. Without this marker the SHA would be a lie |
| `compiled` | build time, UTC | When the metadata was generated |
| `rustc` / `target` | the compiler and `TARGET` | Reproducing a platform-specific bug |

All of it is captured by `crates/dmac-config/build.rs` into `cargo:rustc-env`
variables and read with `env!`, so every field is a `&'static str` baked into the
binary. There is nothing to ship alongside and nothing to read at runtime.

## Why the commit count is the build number

It goes only upwards, it is the same number for every person and every CI runner
with the same history, and it needs no counter file that someone will forget to
commit. `0.1.0 build 42` is unambiguous, and `git rev-list --count` reproduces it
from any checkout.

## Where to see it

- `dmac --version` — version plus the build block.
- `dmac --build-info` — the same block, product name included. This is what to
  paste into a bug report.
- The startup splash, for about two seconds. `--no-splash` skips it.
- The settings screen (once `dmac-config` grows one).

## Semantic versioning policy

Pre-1.0, so `0.MINOR.PATCH`:

- **MINOR** for a user-visible feature or a breaking change to config, keymap or
  session file formats.
- **PATCH** for fixes and internal work.
- **1.0** when the file operation engine has shipped and been trusted with real
  data for a while. Not before: a file manager's 1.0 is a promise about not
  losing files.

Every persisted format (`config.toml`, `session.toml`, `workspace.json`) carries
its own `version` field, independent of the application version. An older binary
reading a newer file degrades; a newer binary migrates and backs up the original.

## The one caveat: build.rs does not rerun on every build

`dmac-config` sits at the bottom of the layering, so invalidating it rebuilds the
entire workspace. Forcing `build.rs` to rerun on every `cargo build` would mean
paying a full rebuild every time, just to refresh a timestamp. That trade is not
worth it.

Instead it reruns when `.git/HEAD` or `.git/index` changes — that is, on a new
commit, a branch switch, or a `git add`. So:

- **Release builds are always exact**, because CI builds from a clean checkout.
- **During development the timestamp can lag** behind the last edit. The `dirty`
  marker is what tells you the SHA does not describe the source; trust that over
  the timestamp.

`cargo clean -p dmac-config` forces a refresh if you need one.

## Building without git

`build.rs` never fails and never panics. If `git` is missing or the source is an
unpacked tarball, the git fields read `unknown` and the build succeeds. Saying
`unknown` is the point — it is better than inventing a commit that does not exist.
