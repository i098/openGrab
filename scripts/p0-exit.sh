#!/usr/bin/env bash
# Phase 0 exit test, guest join. Usage: scripts/p0-exit.sh <meet-link> [p0-exit options]
# Runs the non-headless Chromium inside Xvfb (plan risk R1). See crates/parley-join/README.md.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
build=$HOME/.cargo/bin/blaze
[ -x "$build" ] || build=cargo # blaze is the house builder; cargo only when it is not installed
exec xvfb-run -a -s "-screen 0 1280x800x24" "$build" run --release -p parley-join --bin p0_exit -- "$@"
