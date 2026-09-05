#!/usr/bin/env bash
# Build and run DMACommander in release mode.
#
#   ./run-release.sh                 # left panel = cwd, right = home
#   ./run-release.sh crates docs     # explicit starting directories
#   ./run-release.sh --cursor software
#
# Anything you pass is handed straight to `dmac`, so `--help` works here too.
set -euo pipefail
cd "$(dirname "$0")"

cargo build --release --quiet

# exec, so the process replaces this script: Ctrl-C and the exit status behave
# exactly as if you had run the binary yourself, and there is no stray shell
# sitting between your terminal and the TUI.
exec ./target/release/dmac "$@"
