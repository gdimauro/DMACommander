#!/usr/bin/env bash
# Build and run DMACommander in debug mode.
#
#   ./run-debug.sh                   # left panel = cwd, right = home
#   ./run-debug.sh crates docs
#
# Differences from run-release.sh, all of them about seeing what went wrong:
#
#   * debug assertions are on, so an arithmetic overflow panics loudly instead
#     of wrapping silently — which is how the panel-width bugs were caught;
#   * a full backtrace on panic;
#   * stderr goes to a log file rather than to the screen. A TUI owns the
#     terminal, so anything written to stderr while it runs corrupts the
#     display. This is the single most annoying thing about debugging a
#     terminal application, and redirecting is the whole fix.
set -euo pipefail
cd "$(dirname "$0")"

LOG="${DMAC_LOG:-/tmp/dmac-debug.log}"

cargo build --quiet

export RUST_BACKTRACE=full
export RUST_LOG="${RUST_LOG:-debug}"

echo "stderr -> $LOG   (tail -f it from another terminal)" >&2
: > "$LOG"

# The panic hook restores the terminal before the message is printed, so a
# crash lands on a usable screen; the copy in the log is for after the fact.
exec ./target/debug/dmac "$@" 2> >(tee -a "$LOG" >&2)
